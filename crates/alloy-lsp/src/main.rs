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
//!   compiles to `.d.luau` in the cache directory first. A relative path
//!   reads from the workspace root. Repeatable. The project's own list
//!   joins them: every `.d.aly` under `[build] in` and each
//!   `[flux] definitions` entry, the list `alloy flux` reads. Every
//!   `impl` on a foreign type under the root is injected into the
//!   definitions that declare the target.
//! - `--docs <path>`: the API docs JSON for the child, for hover text.
//! - `--old-solver`: do not pass `--flag:LuauSolverV2=true`.
//! - `--log-level <level>`: what stderr shows: `off`, `error`, `warn`,
//!   `info`, `debug`, or `trace`. Default: `ALLOY_LSP_LOG`, then `warn`.
//! - `--log`: the same as `--log-level trace`.
//! - Everything after `--` goes to the child as is.

mod block_end;
mod components;
mod config_aly;
mod context;
mod doc;
mod extensions;
mod imports;
mod ingots;
mod keywords;
mod log;
mod markup;
mod names;
mod proxy;
mod rpc;
mod settings;
mod tokens;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};

fn main() -> ExitCode {
    alloy_syntax::parser::run_with_deep_stack(run)
}

fn run() -> ExitCode {
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
    let mut workspace_root: Option<PathBuf> = None;

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
        // A path the editor setting gives reads from the workspace, as
        // `[flux] definitions` reads from the project root.
        for d in &mut definitions {
            if d.is_relative() {
                *d = root.join(&*d);
            }
        }

        for d in workspace_definitions(&root) {
            if !definitions
                .iter()
                .any(|p| alloy::modules::normalize(p) == d)
            {
                definitions.push(d);
            }
        }

        exts = extensions::collect(&workspace_files(&root, |n| {
            n.ends_with(".aly") && !n.ends_with(".d.aly")
        }));
        workspace_root = Some(root);
    }

    let config = workspace_root
        .as_deref()
        .and_then(|r| alloy::config::Config::find_within(r, r))
        .and_then(|p| alloy::config::Config::load(&p).ok());
    // `[flux] new_solver = false` in the root's alloy.toml runs the old
    // solver, the way `--old-solver` does.
    let new_solver = new_solver && !config.as_ref().is_some_and(|c| !c.flux.new_solver);

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

    // The rig types `Player.Character`. The editor's `rig` setting wins
    // over `[roblox] rig` in alloy.toml.
    let rig = editor_options
        .get("rig")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| config.as_ref().map(|c| c.roblox.rig.clone()))
        .unwrap_or_else(|| "R15".to_string());
    let mut injected = std::collections::HashSet::new();

    let mut given: Vec<PathBuf> = Vec::new();
    // The file the child reads for each `.d.aly`, and the `.d.aly`: a
    // report on the one goes to the other.
    let mut sources: Vec<(PathBuf, PathBuf)> = Vec::new();

    for path in &definitions {
        match prepare_definitions(path, workspace_root.as_deref()).and_then(|p| {
            extensions::apply(&p, &exts, &rig, &mut injected, workspace_root.as_deref())
        }) {
            Ok(p) => {
                child_args.push(format!("--definitions={}", p.display()));

                if path.to_string_lossy().ends_with(".d.aly") {
                    sources.push((p.clone(), path.clone()));
                }

                given.push(p);
            }

            Err(e) => log::error(&format!("definitions {}: {e}", path.display())),
        }
    }

    match extensions::primitives_file(&exts, &mut injected, workspace_root.as_deref()) {
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

    if let Some(docs) = &docs {
        child_args.push(format!("--docs={docs}"));
    }

    child_args.extend(passthrough);

    let mut child = match Command::new(&child_path)
        .args(&child_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    // The child's stderr still goes to ours; the last line of it names
    // what went wrong when the child dies.
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let stderr_reader = child.stderr.take().map(|child_err| {
        let tail = Arc::clone(&stderr_tail);

        alloy_syntax::parser::spawn_deep(move || {
            for line in BufReader::new(child_err).lines().map_while(Result::ok) {
                eprintln!("{line}");

                if !line.trim().is_empty() {
                    *tail.lock().expect("stderr tail") = line;
                }
            }
        })
    });

    let server = Arc::new(proxy::Server::new(
        Box::new(child_in),
        Box::new(std::io::stdout()),
        exts,
        docs.map(PathBuf::from),
    ));
    {
        let mut st = server.state.lock().expect("state");
        st.definitions = given;
        st.definition_sources = sources;
    }

    // Child -> editor on its own thread.
    let reader_server = Arc::clone(&server);
    let child_name = child_path.clone();
    let _reader = alloy_syntax::parser::spawn_deep(move || {
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

        // The child is gone and every request the editor has out with
        // it. A shutdown expects this; otherwise the editor hears what
        // died and restarts the server, instead of waiting forever.
        if reader_server
            .stopping
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }

        // Stdout can close before the stderr thread reads the last
        // line, and the report then lost the reason. A grandchild may
        // hold stderr open, so the wait has a bound.
        if let Some(reader) = stderr_reader {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);

            while !reader.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }

            if reader.is_finished() {
                let _ = reader.join();
            }
        }

        let last = stderr_tail.lock().expect("stderr tail").clone();
        let text = match last.is_empty() {
            true => format!("luau-lsp ({child_name}) exited"),

            false => format!("luau-lsp ({child_name}) exited: {last}"),
        };

        log::error(&text);
        reader_server.to_client(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "window/showMessage",
            "params": { "type": 1, "message": text },
        }));
        std::process::exit(1);
    });

    // The project's folders on their own thread: an editor that sends
    // no watcher notification still hears about a package install.
    let poll_server = Arc::clone(&server);
    alloy_syntax::parser::spawn_deep(move || poll_server.poll_files());

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

    // The editor is done with us, so the child's own exit is expected.
    server
        .stopping
        .store(true, std::sync::atomic::Ordering::Relaxed);

    // The child gets a second to leave on its own; a child still
    // loading its definitions never answered the shutdown, and the
    // editor kills a server that lingers.
    let gone = std::time::Instant::now() + std::time::Duration::from_secs(1);

    while std::time::Instant::now() < gone {
        match child.try_wait() {
            Ok(Some(_)) => break,

            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),

            Err(_) => break,
        }
    }

    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }

    let _ = std::io::stdout().flush();

    ExitCode::SUCCESS
}

/// The definitions of a workspace: the list `alloy flux` reads when
/// the root holds a configuration, else every `.d.aly` under the root.
fn workspace_definitions(root: &Path) -> Vec<PathBuf> {
    let project = alloy::config::Config::find_within(root, root)
        .and_then(|p| alloy::config::Config::load(&p).ok().map(|c| (p, c)));

    match project {
        Some((path, config)) => {
            alloy::build::definition_files(path.parent().unwrap_or(root), &config)
                .iter()
                .map(|p| alloy::modules::normalize(p))
                .collect()
        }

        None => workspace_files(root, |name| name.ends_with(".d.aly")),
    }
}

/// Every file under a workspace root whose name passes `keep`, outside
/// the build output, `.git`, `node_modules`, and `target`.
fn workspace_files(root: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    // The climb stops at the workspace root, so a project never reads
    // the build directory of a sibling under the same parent.
    let out = alloy::config::Config::find_within(root, root).and_then(|p| {
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
/// `.d.luau` in the workspace's cache directory; anything else passes
/// as is. The name carries the whole path, so `client/types.d.aly` and
/// `server/types.d.aly` write two files.
fn prepare_definitions(path: &Path, root: Option<&Path>) -> Result<PathBuf, String> {
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
    let dir = extensions::cache_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().replace(".d.aly", ""))
        .unwrap_or_else(|| "definitions".to_string());
    let target = dir.join(format!("{stem}-{}.d.luau", proxy::root_key(Some(path))));
    std::fs::write(&target, out.check).map_err(|e| e.to_string())?;

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `.d.aly` of one file name wrote one compiled copy, and the
    /// second overwrote the first.
    #[test]
    fn two_declaration_files_of_one_name_compile_to_two_copies() {
        let dir = std::env::temp_dir().join(format!("alloy-defs-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("client")).unwrap();
        std::fs::create_dir_all(dir.join("server")).unwrap();
        std::fs::write(
            dir.join("client/types.d.aly"),
            "declare client_fn: number\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("server/types.d.aly"),
            "declare server_fn: number\n",
        )
        .unwrap();

        let client = prepare_definitions(&dir.join("client/types.d.aly"), Some(&dir)).unwrap();
        let server = prepare_definitions(&dir.join("server/types.d.aly"), Some(&dir)).unwrap();

        assert_ne!(client, server);
        assert!(
            std::fs::read_to_string(&client)
                .unwrap()
                .contains("client_fn")
        );
        assert!(
            std::fs::read_to_string(&server)
                .unwrap()
                .contains("server_fn")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
