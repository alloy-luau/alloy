//! The type check of `alloy flux`: luau-lsp over the check artifact.
//!
//! The check artifact keeps the source's lines, so a checker error on
//! line 12 of the output is on line 12 of the source; the column maps
//! through the span map. The artifacts go into a mirror of the project
//! under the temp directory, laid out as the build output is, since
//! the emitted requires are relative to that: the runtime at the output
//! root, the root's Luau configuration, and a link to every other
//! folder. The language server does the same for open files.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{Config, FluxConfig};
use crate::render::SpanMap;

/// One source with its check artifact, for the analyzer.
#[derive(Debug)]
pub struct CheckSource {
    /// The source path relative to `[build] in`.
    pub rel: PathBuf,
    pub source: String,
    pub check: String,
    pub map: SpanMap,
}

/// One report of the checker, on a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDiag {
    /// The source path relative to `[build] in`.
    pub rel: PathBuf,
    /// One-based.
    pub line: usize,
    pub col: usize,
    /// `TypeError`, `SyntaxError`, or a lint name such as `LocalUnused`.
    pub kind: String,
    pub message: String,
}

impl TypeDiag {
    /// A type or syntax error, as opposed to one of the checker's lints.
    pub fn is_error(&self) -> bool {
        matches!(self.kind.as_str(), "TypeError" | "SyntaxError")
    }
}

/// What the run found, and what it had to say about its setup.
#[derive(Debug, Default)]
pub struct Analysis {
    pub diagnostics: Vec<TypeDiag>,
    pub notes: Vec<String>,
}

/// The luau-lsp binary: `[flux] luau_lsp`, `ALLOY_LUAU_LSP`, the PATH,
/// then `~/.alloy/bin` and `~/.ember/bin`.
pub fn find_luau_lsp(config: &FluxConfig) -> Option<PathBuf> {
    if let Some(p) = &config.luau_lsp {
        let p = PathBuf::from(p);

        return p.is_file().then_some(p);
    }

    if let Ok(p) = std::env::var("ALLOY_LUAU_LSP") {
        let p = PathBuf::from(p);

        if p.is_file() {
            return Some(p);
        }
    }

    let name = if cfg!(windows) {
        "luau-lsp.exe"
    } else {
        "luau-lsp"
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".alloy/bin"));
        dirs.push(home.join(".ember/bin"));
    }

    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

const TYPES_URL: &str = "https://luau-lsp.pages.dev/type-definitions";

/// The Roblox globals: the luau-lsp extension's copy, the Alloy
/// extension's copy, or one downloaded into `~/.alloy/types`. `None`
/// with a note when none can be had.
pub fn roblox_definitions(config: &FluxConfig, notes: &mut Vec<String>) -> Option<PathBuf> {
    let file = format!("globalTypes.{}.d.luau", config.security_level);
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".alloy/types").join(&file));
    }

    if let Some(cfg) = dirs::config_dir() {
        for editor in ["Code", "Code - Insiders", "VSCodium", "Cursor"] {
            let storage = cfg.join(editor).join("User/globalStorage");
            candidates.push(storage.join("johnnymorganz.luau-lsp").join(&file));
            candidates.push(storage.join("andrewbordis.alloy-luau").join(&file));
        }
    }

    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Some(found.clone());
    }

    let Some(home) = dirs::home_dir() else {
        notes.push("no home directory for the Roblox types; the globals are unknown".to_string());

        return None;
    };
    let dir = home.join(".alloy/types");
    let target = dir.join(&file);

    if std::fs::create_dir_all(&dir).is_err() {
        notes.push(format!(
            "cannot create {}; the Roblox globals are unknown",
            dir.display()
        ));

        return None;
    }

    let url = format!("{TYPES_URL}/{file}");
    let status = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&target)
        .arg(&url)
        .status();

    match status {
        Ok(s) if s.success() && target.is_file() => {
            notes.push(format!("fetched {file} into {}", dir.display()));

            Some(target)
        }

        _ => {
            let _ = std::fs::remove_file(&target);
            notes.push(format!(
                "cannot fetch {url}; put the file at {} or set `[flux] roblox_types = false`",
                target.display()
            ));

            None
        }
    }
}

/// A mirror of the project for the analyzer: the check artifacts under
/// `in`, the runtime under `out`, the Luau configuration, and a link to
/// every other entry of the root.
fn mirror_dir(root: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);

    // `root` sits one level down, so a `[build] in` of `../x` stays
    // inside the mirror.
    std::env::temp_dir()
        .join(format!("alloy-flux-{:016x}", h.finish()))
        .join("root")
}

fn link_entry(from: &Path, to: &Path) {
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(from, to);
    }

    #[cfg(windows)]
    {
        if from.is_dir() {
            let _ = std::os::windows::fs::symlink_dir(from, to);
        } else {
            let _ = std::os::windows::fs::symlink_file(from, to);
        }
    }
}

/// Runs the analyzer over the check artifacts and maps what it says
/// onto the sources.
pub fn analyze(root: &Path, config: &Config, files: &[CheckSource]) -> Result<Analysis, String> {
    let mut analysis = Analysis::default();
    let Some(binary) = find_luau_lsp(&config.flux) else {
        return Err("luau-lsp is not on the PATH; `[flux] luau_lsp` names the binary, `typecheck = false` skips the check".to_string());
    };

    let mirror = mirror_dir(root);

    if let Some(parent) = mirror.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }

    std::fs::create_dir_all(&mirror).map_err(|e| format!("{}: {e}", mirror.display()))?;

    // Everything of the root but the sources and the output, linked, so
    // a package folder and its `.luaurc` resolve.
    for entry in std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = entry.file_name();
        let skip = [".git", "target", "node_modules"]
            .iter()
            .any(|s| name == *s)
            || Path::new(&name) == config.build.input
            || Path::new(&name) == config.build.out
            || Path::new(&name) == config.test.out;

        if !skip {
            link_entry(&entry.path(), &mirror.join(&name));
        }
    }

    if !root.join(".luaurc").is_file() && !root.join(".config.luau").is_file() {
        let c = crate::luau_config::LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: vec![(
                "alloy".to_string(),
                format!(
                    "./{}/alloy",
                    config.build.out.to_string_lossy().replace('\\', "/")
                ),
            )],
        };
        let _ = std::fs::write(
            mirror.join(".luaurc"),
            crate::luau_config::render_luaurc(&c),
        );
    }

    let out = mirror.join(&config.build.out);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    std::fs::write(out.join("alloy.luau"), crate::RUNTIME).map_err(|e| e.to_string())?;

    let mut sources: Vec<PathBuf> = Vec::new();
    let mut definitions: Vec<PathBuf> = Vec::new();

    if config.flux.roblox_types
        && let Some(p) = roblox_definitions(&config.flux, &mut analysis.notes)
    {
        definitions.push(p);
    }

    for d in &config.flux.definitions {
        definitions.push(root.join(d));
    }

    // The artifacts sit where the build would put them, so `./x` and
    // `../alloy` resolve.
    for f in files {
        let Some(rel_out) = crate::build::output_for(&f.rel) else {
            continue;
        };
        let target = out.join(&rel_out);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        std::fs::write(&target, &f.check).map_err(|e| e.to_string())?;

        if rel_out.to_string_lossy().ends_with(".d.luau") {
            definitions.push(target);
        } else {
            sources.push(config.build.out.join(&rel_out));
        }
    }

    if sources.is_empty() {
        return Ok(analysis);
    }

    // An extension on a foreign type reaches the checker through the
    // definitions, as it does in the editor.
    let exts: Vec<crate::extensions::Extension> = files
        .iter()
        .filter(|f| !f.rel.to_string_lossy().ends_with(".d.aly"))
        .flat_map(|f| crate::extensions::collect(&f.source))
        .collect();
    let ext_dir = mirror.join(".alloy-ext");
    let mut injected = std::collections::HashSet::new();
    let mut with_exts = Vec::new();

    for d in &definitions {
        match crate::extensions::apply(d, &exts, &mut injected, &ext_dir) {
            Ok(p) => with_exts.push(p),

            Err(e) => analysis
                .notes
                .push(format!("definitions {}: {e}", d.display())),
        }
    }

    match crate::extensions::primitives_file(&exts, &mut injected, &ext_dir) {
        Ok(Some(p)) => with_exts.push(p),

        Ok(None) => {}

        Err(e) => analysis.notes.push(format!("primitive extensions: {e}")),
    }

    let definitions = with_exts;

    let mut cmd = Command::new(&binary);
    cmd.current_dir(&mirror)
        .arg("analyze")
        .arg("--flag:LuauSolverV2=true");

    for d in &definitions {
        cmd.arg(format!("--definitions={}", d.display()));
    }

    let sourcemap = root.join(".alloy/sourcemap.json");

    if sourcemap.is_file() {
        cmd.arg("--sourcemap").arg(&sourcemap);
    }

    for s in &sources {
        cmd.arg(s);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", binary.display()))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);

    // A message may run over several lines; the extra lines join the
    // report before them.
    let mut last: Option<usize> = None;

    for line in text.lines() {
        let Some(report) = parse_line(line) else {
            if let Some(i) = last
                && !line.starts_with('[')
                && !line.trim().is_empty()
            {
                let d = &mut analysis.diagnostics[i];
                d.message.push(' ');
                d.message.push_str(line.trim());
            }

            continue;
        };
        last = None;
        let (line_no, col, kind, message) = (report.line, report.col, report.kind, report.message);

        // The layout lints read the emit, not the source.
        if message.contains("Unknown require")
            || matches!(kind, "SameLineStatement" | "MultiLineStatement")
        {
            continue;
        }

        // The path is relative to the mirror: `<out>/a/b.luau`.
        let path = PathBuf::from(report.path.trim_start_matches("./"));
        let path = path
            .strip_prefix(&mirror)
            .map(Path::to_path_buf)
            .unwrap_or(path);
        let Ok(rel_out) = path.strip_prefix(&config.build.out) else {
            continue;
        };
        let Some(f) = files
            .iter()
            .find(|f| crate::build::output_for(&f.rel).as_deref() == Some(rel_out))
        else {
            continue;
        };

        let is_error = kind == "TypeError" || kind == "SyntaxError";
        let Some(mapped) = map_position(f, line_no, col, is_error, message) else {
            continue;
        };

        analysis.diagnostics.push(TypeDiag {
            rel: f.rel.clone(),
            line: mapped.0,
            col: mapped.1,
            kind: kind.to_string(),
            message: message.to_string(),
        });
        last = Some(analysis.diagnostics.len() - 1);
    }

    analysis
        .diagnostics
        .sort_by(|a, b| (&a.rel, a.line, a.col).cmp(&(&b.rel, b.line, b.col)));
    analysis.diagnostics.dedup();

    Ok(analysis)
}

/// One line of the analyzer's output, split.
struct Line<'a> {
    path: &'a str,
    line: usize,
    col: usize,
    kind: &'a str,
    message: &'a str,
}

/// `path(line,col): Kind: message`; `None` for any other line.
fn parse_line(line: &str) -> Option<Line<'_>> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let (l, c) = line[open + 1..close].split_once(',')?;
    let rest = line[close + 1..].strip_prefix(": ")?;
    let (kind, message) = rest.split_once(": ")?;

    if !kind.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }

    Some(Line {
        path: &line[..open],
        line: l.trim().parse().ok()?,
        col: c.trim().parse().ok()?,
        kind,
        message,
    })
}

/// The source position of an output position: the same line, the
/// column through the map. `None` drops the report: a silenced line, or
/// a warning about generated text.
fn map_position(
    f: &CheckSource,
    line: usize,
    col: usize,
    is_error: bool,
    message: &str,
) -> Option<(usize, usize)> {
    let silence = crate::directives::scan(&f.source);

    if !silence.allows(line.saturating_sub(1)) {
        return None;
    }

    let out_off = offset_of(&f.check, line, col)?;

    if !is_error && f.map.is_generated(out_off as u32) {
        return None;
    }

    // `$nameof(x)` and `$stringify(x)` turn their argument into a
    // string, so the checker sees no use of `x` where the source has one.
    if let Some(name) = unused_name(message)
        && consumed_by_intrinsic(&f.source, name)
    {
        return None;
    }

    let src_off = f.map.to_source(out_off as u32) as usize;
    let (sl, sc) = line_col(&f.source, src_off);

    if sl == line {
        Some((line, sc))
    } else {
        Some((line, 1))
    }
}

/// The variable of a `LocalUnused` or `FunctionUnused` lint.
fn unused_name(message: &str) -> Option<&str> {
    let rest = message
        .strip_prefix("Variable '")
        .or_else(|| message.strip_prefix("Function '"))?;

    rest.split('\'').next()
}

/// True when `$nameof(` or `$stringify(` names the variable in its
/// argument.
fn consumed_by_intrinsic(source: &str, name: &str) -> bool {
    for sigil in ["$nameof(", "$stringify("] {
        let mut from = 0;

        while let Some(i) = source[from..].find(sigil) {
            let start = from + i + sigil.len();
            let argument = source[start..]
                .split_once(')')
                .map(|(a, _)| a)
                .unwrap_or(&source[start..]);
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let found = argument.match_indices(name).any(|(at, _)| {
                let before = argument[..at].chars().next_back();
                let after = argument[at + name.len()..].chars().next();

                !before.is_some_and(is_word) && !after.is_some_and(is_word)
            });

            if found {
                return true;
            }

            from = start;
        }
    }

    false
}

fn offset_of(text: &str, line: usize, col: usize) -> Option<usize> {
    let mut at = 0;

    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            return Some(at + col.saturating_sub(1).min(l.len()));
        }

        at += l.len();
    }

    None
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, str::len) + 1;

    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_analyzer_line_parses() {
        let d = parse_line("src/a.luau(3,12): TypeError: Expected 'number', got 'string'").unwrap();
        assert_eq!(d.path, "src/a.luau");
        assert_eq!((d.line, d.col), (3, 12));
        assert_eq!(d.kind, "TypeError");
        assert!(d.message.starts_with("Expected"));
        assert!(parse_line("[INFO] Loading definitions file").is_none());
    }

    #[test]
    fn a_position_maps_through_the_artifact() {
        let src = "local x: number = \"s\"\n";
        let out = crate::compile(src).unwrap();
        let f = CheckSource {
            rel: PathBuf::from("a.aly"),
            source: src.to_string(),
            check: out.check.clone(),
            map: out.map,
        };
        assert_eq!(map_position(&f, 1, 19, true, "Expected"), Some((1, 19)));
        let silenced = CheckSource {
            source: "local x: number = \"s\" --@alloy-ignore\n".to_string(),
            ..f
        };
        assert_eq!(map_position(&silenced, 1, 19, true, "Expected"), None);
        assert!(consumed_by_intrinsic(
            "local RunService = 1\nlocal f = $nameof(RunService.Heartbeat)\n",
            "RunService"
        ));
        assert!(!consumed_by_intrinsic(
            "local f = $nameof(MyRunService)\n",
            "RunService"
        ));
    }
}
