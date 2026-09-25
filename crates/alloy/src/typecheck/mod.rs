//! The type check of `alloy flux`: luau-lsp over the check artifact.
//!
//! The check artifact keeps the source's lines, so a checker error on
//! line 12 of the output is on line 12 of the source; the column maps
//! through the span map. The artifacts go into a mirror of the project
//! under the temp directory, laid out as the build output is, since
//! the emitted requires are relative to that: the runtime at the output
//! root, the root's Luau configuration, and a link to every other
//! folder. The language server does the same for open files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{Config, FluxConfig};
use crate::render::SpanMap;

mod messages;

pub use messages::*;

/// One source with its check artifact, for the analyzer.
#[derive(Debug)]
pub struct CheckSource {
    /// The source path relative to `[build] in`.
    pub rel: PathBuf,
    pub source: String,
    pub check: String,
    pub map: SpanMap,
    /// Every lint that fired, as its one-based line and its name. The
    /// checker has a lint of its own for some of them, and one problem
    /// reads once.
    pub lint_lines: Vec<(usize, &'static str)>,
    /// One-based lines that carry a compiler diagnostic; the checker's
    /// reports there describe an unreliable emit and stay out.
    pub error_lines: Vec<usize>,
    /// Whether the parser read the whole file. Past its first error the
    /// parser invents the tree and the emit copies the text through, so
    /// every type error the checker finds is about code no one wrote.
    /// The parse error is the one thing to fix first.
    pub parsed_clean: bool,
    /// Zero-based lines an `--@alloy-expect-error` covers that the
    /// compiler or a lint reported on.
    pub expected_hits: Vec<usize>,
}

/// One report of the checker, on a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDiag {
    /// The source path relative to `[build] in`.
    pub rel: PathBuf,
    /// One-based.
    pub line: usize,
    pub col: usize,
    /// `TypeError`, `SyntaxError`, `UnknownModule`, `DirectiveError`, or
    /// a lint name such as `LocalUnused`.
    pub kind: String,
    pub message: String,
}

impl TypeDiag {
    /// The book section the kind belongs to, for a report Alloy raised
    /// itself; `None` leaves the report to the checker's own `luau`.
    pub fn code(&self) -> Option<&'static str> {
        section_of(&self.kind)
    }

    /// A type or syntax error, as opposed to one of the checker's lints.
    pub fn is_error(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "TypeError"
                | "SyntaxError"
                | "UnknownModule"
                | "DirectiveError"
                | "StructError"
                | "BoundError"
                | "EnumError"
                | "ExhaustiveMatch"
        )
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

    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file() && answers_version(p))
}

/// Whether a candidate is the analyzer and not a stand-in for it: a
/// toolchain manager leaves a shim under the name that fails every run
/// when the tool is not in its manifest, `--version` first.
fn answers_version(binary: &Path) -> bool {
    Command::new(binary)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The oldest luau-lsp that checks the lowered Alloy clean. 1.68.0
/// finds no one type for an array rest, `local [head, ...rest] = xs`.
const MIN_LUAU_LSP: (u32, u32, u32) = (1, 69, 0);

/// A note when the analyzer is older than [`MIN_LUAU_LSP`].
fn old_analyzer_note(binary: &Path) -> Option<String> {
    let out = Command::new(binary).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.split_whitespace().last()?.trim_start_matches('v');
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    let found = (parts.next()??, parts.next()??, parts.next()??);
    let (a, b, c) = MIN_LUAU_LSP;

    (found < MIN_LUAU_LSP).then(|| {
        format!(
            "luau-lsp {version} is older than {a}.{b}.{c}; update it, or the check can report errors the code does not have"
        )
    })
}

/// Why a run that reported nothing failed, if it did. The analyzer's
/// own progress lines are not trouble; anything else it said is, and so
/// is a non-zero exit with nothing to say.
fn analyzer_trouble(status: &std::process::ExitStatus, stderr: &str) -> Option<String> {
    const NOISE: [&str; 4] = ["WARNING:", "[WARN]", "[INFO]", "[TRACE]"];

    match stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !NOISE.iter().any(|n| l.starts_with(n)))
    {
        Some(line) => Some(line.to_string()),

        None => {
            (!status.success()).then(|| format!("it exited with {}", status.code().unwrap_or(-1)))
        }
    }
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
/// every other entry of the root. `above` is how many folders above the
/// root the mirror holds, so a dependency at `../../x` stays inside it.
fn mirror_dir(root: &Path, above: usize) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);

    let mut dir = std::env::temp_dir().join(format!("alloy-flux-{:016x}", h.finish()));

    for _ in 1..above {
        dir.push("up");
    }

    dir.join("root")
}

/// A path with `.` and `..` folded, no file system access.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                out.pop();
            }

            other => out.push(other),
        }
    }

    out
}

/// `path` relative to the folder `root`, both absolute: `..` for each
/// folder of `root` past the common part, then the rest of `path`.
fn relative_to(root: &Path, path: &Path) -> PathBuf {
    let root: Vec<_> = root.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = root.iter().zip(&path).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();

    for _ in common..root.len() {
        out.push("..");
    }

    for c in &path[common..] {
        out.push(c);
    }

    out
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
/// onto the sources. `deps` holds the artifacts of the projects the
/// imports lead into, by absolute output path; each lands in the
/// mirror where it sits beside the root.
pub fn analyze(
    root: &Path,
    config: &Config,
    files: &[CheckSource],
    deps: &[(PathBuf, String)],
) -> Result<Analysis, String> {
    let mut analysis = Analysis::default();
    let Some(binary) = find_luau_lsp(&config.flux) else {
        return Err("luau-lsp is not on the PATH; `[flux] luau_lsp` names the binary, `typecheck = false` skips the check".to_string());
    };

    if let Some(note) = old_analyzer_note(&binary) {
        analysis.notes.push(note);
    }

    let root_abs = normalize(&std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()));
    let placed: Vec<(PathBuf, &str)> = deps
        .iter()
        .map(|(path, text)| (relative_to(&root_abs, path), text.as_str()))
        .collect();
    let above = placed
        .iter()
        .map(|(rel, _)| {
            rel.components()
                .take_while(|c| *c == std::path::Component::ParentDir)
                .count()
        })
        .max()
        .unwrap_or(1);
    let mirror = mirror_dir(root, above);
    let base = mirror
        .ancestors()
        .nth(above.max(1))
        .unwrap_or(&mirror)
        .to_path_buf();
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&mirror).map_err(|e| format!("{}: {e}", mirror.display()))?;

    for (rel, text) in &placed {
        let target = normalize(&mirror.join(rel));

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        std::fs::write(&target, text).map_err(|e| e.to_string())?;
    }

    // Everything of the root but the sources and the output, linked, so
    // a package folder and its `.luaurc` resolve.
    for entry in std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = entry.file_name();
        let skip = [
            ".git",
            "target",
            "node_modules",
            ".luaurc",
            ".config.luau",
            "sourcemap.json",
        ]
        .iter()
        .any(|s| name == *s)
            || Path::new(&name) == config.build.input
            || Path::new(&name) == config.build.out
            || Path::new(&name) == config.test.out;

        if !skip {
            link_entry(&entry.path(), &mirror.join(&name));
        }
    }

    // The mirror lays the artifacts out as the output, so an alias that
    // names a folder under `in` points at its output here.
    let mut luau = crate::luau_config::read_dir(root)
        .map(|(_, c)| c)
        .unwrap_or_default();

    if luau.language_mode.is_none() {
        luau.language_mode = Some("strict".to_string());
    }

    // The mount table serves aliases too while `[project] mount_aliases`
    // stays on; the mirror's config carries them so the analyzer
    // resolves `@pkg/x` the way the compiler does. The user's own
    // file is never written.
    if config.project.mount_aliases {
        for (name, m) in &config.mount {
            if !luau.aliases.iter().any(|(a, _)| a == name) {
                luau.aliases
                    .push((name.clone(), format!("./{}", m.0.replace('\\', "/"))));
            }
        }
    }

    let input_abs = normalize(&root.join(&config.build.input));

    for (_, target) in &mut luau.aliases {
        let abs = normalize(&root.join(target.as_str()));

        if let Ok(rest) = abs.strip_prefix(&input_abs) {
            let mapped = config.build.out.join(rest);
            *target = format!("./{}", mapped.to_string_lossy().replace('\\', "/"));
        }
    }

    if !luau.aliases.iter().any(|(a, _)| a == "alloy") {
        luau.aliases.push((
            "alloy".to_string(),
            format!(
                "./{}/alloy",
                config.build.out.to_string_lossy().replace('\\', "/")
            ),
        ));
    }

    let _ = std::fs::write(
        mirror.join(".luaurc"),
        crate::luau_config::render_luaurc(&luau),
    );

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

    // A `.d.aly` under `in` reaches the checker with the sources below.
    // One outside it compiles here, and a report on it names its file.
    let input_dir = normalize(&root.join(&config.build.input));
    // Every compiled `.d.aly`: the path a report names, the artifact,
    // and the source. The ones that name each other reach the checker
    // as one file.
    let mut declared: Vec<(PathBuf, String, String)> = Vec::new();

    for d in &config.flux.definitions {
        let path = normalize(&root.join(d));

        if !d.ends_with(".d.aly") {
            definitions.push(path);

            continue;
        }

        if files.iter().any(|f| input_dir.join(&f.rel) == path) {
            continue;
        }

        let compiled = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|source| {
                let options = crate::EmitOptions {
                    file_name: path.to_string_lossy().into_owned(),
                    definitions: true,
                    ..Default::default()
                };

                crate::compile_with(&source, &options).map_err(|e| e.located(&source))
            });

        match compiled {
            Ok(out) => {
                for d in &out.diagnostics {
                    let source = std::fs::read_to_string(&path).unwrap_or_default();
                    let (line, col) = crate::directives::line_col(&source, d.start as usize);
                    analysis.diagnostics.push(TypeDiag {
                        rel: path.clone(),
                        line,
                        col,
                        kind: crate::docs::kind_for(&d.message).to_string(),
                        message: d.message.clone(),
                    });
                }

                let source = std::fs::read_to_string(&path).unwrap_or_default();
                declared.push((path, out.check, source));
            }

            Err(e) => analysis
                .notes
                .push(format!("definitions {}: {e}", path.display())),
        }
    }

    // A plain `.luau` beside the sources sits in the output too, as the
    // build copies it, so a require of it resolves.
    let input = root.join(&config.build.input);
    let written = crate::build::written_dirs(root, config);
    let mut plain = Vec::new();
    let _ = crate::build::walk_plain(&input, &written, &mut plain);

    for path in plain {
        let rel = path.strip_prefix(&input).unwrap_or(&path);
        let target = out.join(rel);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        if let Ok(text) = std::fs::read(&path) {
            std::fs::write(&target, text).map_err(|e| e.to_string())?;
        }
    }

    // Every data file becomes the module the build writes from it, so
    // `require("./data")` types as its table. A file beside a module of
    // the same stem is left out: the build reports that collision, and
    // the module wins here as it does there.
    let mut data = Vec::new();
    let _ = crate::build::walk_data(&input, &written, &mut data);

    for path in data {
        let rel = path.strip_prefix(&input).unwrap_or(&path);

        if crate::data::module_beside(&path).is_some() {
            continue;
        }

        if let Some(format) = crate::data::Format::of_path(&path)
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(luau) = crate::data::convert(&text, format)
        {
            let target = out.join(rel).with_extension("luau");

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }

            std::fs::write(&target, luau).map_err(|e| e.to_string())?;
        }
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
            declared.push((f.rel.clone(), f.check.clone(), f.source.clone()));
        } else {
            sources.push(config.build.out.join(&rel_out));
        }
    }

    let merged = crate::declarations::merge_definitions(&declared);

    for (i, (text, _)) in merged.iter().enumerate() {
        let target = mirror.join(format!("{DECLARED}{i}.d.luau"));
        std::fs::write(&target, text).map_err(|e| e.to_string())?;
        definitions.push(target);
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
        match crate::extensions::apply(d, &exts, &config.roblox.rig, &mut injected, &ext_dir) {
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
        // A printed type must arrive whole: `friendly_type_message`
        // folds an emitted table back to the name the source wrote, and
        // the default limit cuts it to `*TRUNCATED*` first. The language
        // server raises the same two flags, so both say one thing.
        .arg("--flag:LuauTypeMaximumStringifierLength=200000")
        .arg("--flag:LuauTableTypeMaximumStringifierLength=200000");

    // `[flux] new_solver = false` runs the old solver, which evaluates
    // no type function.
    if config.flux.new_solver {
        cmd.arg("--flag:LuauSolverV2=true");
    }

    for d in &definitions {
        cmd.arg(format!("--definitions={}", d.display()));
    }

    // The sourcemap the language server gives luau-lsp. Its scripts
    // point at the artifacts, which sit under `out` here as the build
    // writes them, so `script.Parent` and a `require` of a child resolve.
    if let Some(text) = crate::project::luau_sourcemap(root, config) {
        let out_dir = normalize(&root.join(&config.build.out));
        let text = crate::project::map_sourcemap(&text, &|s| {
            let luau = crate::project::luau_script_path(s);
            let at = normalize(&root.join(&luau));

            match at.strip_prefix(&input_dir) {
                Ok(rest) if !at.starts_with(&out_dir) => config
                    .build
                    .out
                    .join(rest)
                    .to_string_lossy()
                    .replace('\\', "/"),

                _ => luau,
            }
        });
        let target = mirror.join("sourcemap.json");
        std::fs::write(&target, text).map_err(|e| e.to_string())?;
        cmd.arg("--sourcemap").arg(&target);
    }

    for s in &sources {
        cmd.arg(s);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", binary.display()))?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let text = String::from_utf8_lossy(&output.stdout).into_owned() + &stderr;

    // The analyzer exits 1 when it reports a diagnostic, so the exit
    // code alone says nothing. A run that read no report and had
    // trouble is a binary that cannot analyze, and a clean project
    // reads the same way: the check has to say so instead.
    if !text.lines().any(|line| parse_line(line).is_some())
        && let Some(trouble) = analyzer_trouble(&output.status, &stderr)
    {
        return Err(format!(
            "{} failed and reported nothing: {trouble}; `[flux] luau_lsp` or ALLOY_LUAU_LSP names the binary, `typecheck = false` skips the check",
            binary.display()
        ));
    }

    let known = known_shapes(files);
    // The aliases a message names a folder through, read once.
    let module_aliases = crate::modules::aliases(root, &crate::project::Tree::load(root, config));
    // A message may run over several lines; the extra lines join the
    // report before them.
    let mut last: Option<usize> = None;
    // The lines an `--@alloy-expect-error` covers that the checker
    // reported on, by source; the rest of the directives are errors.
    let mut expected_hits: HashMap<PathBuf, HashSet<usize>> = HashMap::new();
    let mut directives: HashMap<PathBuf, crate::directives::Directives> = HashMap::new();
    // The artifacts the checker could not parse; its lints over one of
    // them describe a partial tree.
    let mut unparsed: HashSet<PathBuf> = HashSet::new();

    // Every report, for a report that repeats one inside its bracket.
    let reports: Vec<Line<'_>> = text.lines().filter_map(parse_line).collect();

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
        if matches!(kind, "SameLineStatement" | "MultiLineStatement") {
            continue;
        }

        // A half-typed member access leaves the checker with no name,
        // and it reports its own stand-in. The parser names the gap.
        if crate::shapes::names_only_the_emit(message) {
            continue;
        }

        // The path is relative to the mirror: `<out>/a/b.luau`.
        let path = PathBuf::from(report.path.trim_start_matches("./"));
        let path = path
            .strip_prefix(&mirror)
            .map(Path::to_path_buf)
            .unwrap_or(path);
        // A merged file of `.d.aly` files, or the copy that carries the
        // extensions: the report names the file alone, so the name it
        // quotes finds the line, and the line finds the file. A name
        // the emit spells another way sits in a source.
        let group = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split(DECLARED).nth(1))
            .and_then(|n| n.strip_suffix(".d.luau"))
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|i| merged.get(i));

        if let Some((text, segments)) = group {
            if quoted_after(message, "Unknown type '").is_some_and(alloy_reports_the_type) {
                continue;
            }

            let place = quoted_place(text, message)
                .and_then(|(line, col)| {
                    crate::declarations::segment_at(segments, line - 1)
                        .map(|(s, at)| (s.source.clone(), at + 1, col))
                })
                .or_else(|| {
                    segments.iter().find_map(|s| {
                        let source = declared.iter().find(|d| d.0 == s.source)?;

                        quoted_place(&source.2, message).map(|(l, c)| (s.source.clone(), l, c))
                    })
                });
            let (rel, line, col) = place.unwrap_or_else(|| {
                (
                    segments
                        .first()
                        .map(|s| s.source.clone())
                        .unwrap_or_default(),
                    1,
                    1,
                )
            });
            analysis.diagnostics.push(TypeDiag {
                rel,
                line,
                col,
                kind: kind.to_string(),
                message: message.to_string(),
            });

            continue;
        }

        let Ok(rel_out) = path.strip_prefix(&config.build.out) else {
            continue;
        };
        let Some(f) = files
            .iter()
            .find(|f| crate::build::output_for(&f.rel).as_deref() == Some(rel_out))
        else {
            continue;
        };

        // A definitions report carries no position: the checker names
        // the file alone. The name the message quotes sits in the
        // artifact, so the report lands on the line that writes it and
        // the map takes that back to the source.
        let (line_no, col) = if line_no == 0 {
            quoted_place(&f.check, message).unwrap_or((1, 1))
        } else {
            (line_no, col)
        };

        // The checker writes a mistake in a match arm again at the `(`
        // of the whole match. The report inside points at the mistake.
        let inner = reports
            .iter()
            .filter(|r| r.path == report.path && r.message == message)
            .map(|r| (r.line, r.col));

        if repeats_an_inner_report(&f.check, line_no, col, inner) {
            continue;
        }

        let silence = directives
            .entry(f.rel.clone())
            .or_insert_with(|| crate::directives::scan(&f.source));

        if silence.expects(line_no.saturating_sub(1)) {
            expected_hits
                .entry(f.rel.clone())
                .or_default()
                .insert(line_no.saturating_sub(1));
        }

        let is_error = kind == "TypeError" || kind == "SyntaxError";

        // An artifact the checker cannot parse leaves it a partial
        // tree, and its lints over that tree call everything unused.
        if kind == "SyntaxError" {
            unparsed.insert(f.rel.clone());
        }

        let Some(mapped) = map_position(f, line_no, col, is_error, kind, message) else {
            continue;
        };

        // Alloy owns the unused-name lints, and its own words name the
        // construct the source wrote. Two lints for one idea disagree
        // inside one run, so the checker's copy goes.
        if owned_lint(kind) {
            continue;
        }

        // Alloy's own lint already said this, in the words of what the
        // source wrote; the checker's copy on the same line says it
        // twice.
        if let Some(names) = paired_lint(kind)
            && f.lint_lines
                .iter()
                .any(|(at, name)| *at == mapped.0 && names.contains(name))
        {
            continue;
        }

        // `:connect` draws a missing-key report as well as
        // `deprecated_method`, which names the current spelling and
        // carries the rewrite. One report per mistake.
        if message.contains("Did you mean")
            && f.lint_lines
                .iter()
                .any(|(at, name)| *at == mapped.0 && *name == "deprecated_method")
        {
            continue;
        }

        if f.error_lines.contains(&mapped.0) {
            continue;
        }

        // The enum emit writes `tag` and `_1`; a report about one of
        // those, on a line the source never wrote them on, describes
        // the emit and names nothing the reader can fix.
        // The same for a duplicate key the source wrote once: a markup
        // attribute that expands to several properties builds one.
        if f.source
            .lines()
            .nth(mapped.0.saturating_sub(1))
            .is_some_and(|text| {
                crate::shapes::names_the_emit_key(message, text)
                    || crate::shapes::duplicate_only_in_the_emit(message, text)
            })
        {
            continue;
        }

        // Past its first error the parser invents the tree and the emit
        // copies the text through: `trait Zap` reads to the checker as
        // a call of an unknown global, and the `end` the recovery never
        // saw is a syntax error of its own. The compiler already names
        // the parse error, and the lints are off for the same reason, so
        // only a type error away from the recovery still stands.
        if !f.parsed_clean
            && (!is_error || kind == "SyntaxError" || message.starts_with("Unknown global"))
        {
            continue;
        }

        // A require the checker could not resolve names what the source
        // asked for; it is an error, as the require fails at runtime.
        let (kind, message) = if message.starts_with("Unknown require") {
            // Alloy writes the runtime require; the reader wrote no
            // import of it, so a report about it names nothing to fix.
            if f.check
                .lines()
                .nth(line_no.saturating_sub(1))
                .and_then(runtime_require_span)
                .is_some_and(|(s, e)| (s..=e).contains(&col.saturating_sub(1)))
            {
                continue;
            }

            // No quoted path on the line means `require(script.Parent)`
            // or another runtime path. It resolves in Roblox, and the
            // `raw_require` lint already says the checker cannot follow
            // it, so there is nothing to report here.
            let Some(spec) = required_spec(message, &f.source, mapped.0.saturating_sub(1)) else {
                continue;
            };
            let rel = config.build.input.join(&f.rel);
            let named = crate::modules::alias_target(&spec, &module_aliases, Some(root));

            (
                "UnknownModule".to_string(),
                unknown_module_message(&spec, &rel, named.as_deref()),
            )
        } else if message.contains(NO_MODULE_RETURN) {
            // The module is there and returns no value. The import is
            // what the reader wrote, so the report names that.
            let Some(spec) = required_spec(message, &f.source, mapped.0.saturating_sub(1)) else {
                continue;
            };

            // A plain `.luau` module has no export table, so its
            // report asks for a `return` alone; the compiler's own
            // check words it the same way.
            let from = root.join(&config.build.input).join(&f.rel);
            let luau = crate::modules::resolve(&spec, &from, &module_aliases)
                .and_then(|p| p.extension().map(|e| e == "luau" || e == "lua"))
                .unwrap_or(false);

            (
                "UnknownModule".to_string(),
                no_module_return_message(&spec, luau),
            )
        } else {
            (kind.to_string(), message.to_string())
        };

        // The rewrite above renames a kind, `Unknown require` into
        // `UnknownModule`; a region that names one reads the name the
        // author sees, so the region is read once more here.
        if let Some(silence) = directives.get(&f.rel)
            && !silence.allows_named(mapped.0.saturating_sub(1), Some(&kind))
        {
            continue;
        }

        analysis.diagnostics.push(TypeDiag {
            rel: f.rel.clone(),
            line: mapped.0,
            col: mapped.1,
            kind,
            message,
        });
        last = Some(analysis.diagnostics.len() - 1);
    }

    for f in files {
        let silence = directives
            .entry(f.rel.clone())
            .or_insert_with(|| crate::directives::scan(&f.source));
        let mut errored: HashSet<usize> = f.expected_hits.iter().copied().collect();

        if let Some(hits) = expected_hits.get(&f.rel) {
            errored.extend(hits);
        }

        for (at, col, reason) in silence.unmet(&errored) {
            analysis.diagnostics.push(TypeDiag {
                rel: f.rel.clone(),
                line: at + 1,
                col: col.max(1),
                kind: "DirectiveError".to_string(),
                message: crate::directives::unmet_message(reason.as_deref()),
            });
        }
    }

    analysis
        .diagnostics
        .retain(|d| d.is_error() || !unparsed.contains(&d.rel));

    // A name may be declared in two files. The file a report sits in is
    // the one whose declaration the reader is looking at, so its own
    // shapes answer first.
    // A private method is not on the struct's public table, so the
    // checker reads a call of one from outside as a member the struct
    // has not got. The resite reads a private member off the shape, the
    // way it reads a private field, so the methods every impl of the
    // project keeps to itself join the shape here.
    let sources: Vec<String> = files.iter().map(|f| f.source.clone()).collect();
    let private_methods = crate::extensions::project_impls(&sources).privates;
    let per_file: Vec<(PathBuf, Vec<crate::declarations::Shape>)> = files
        .iter()
        .map(|f| {
            let mut shapes = shapes_in_reach(&f.source, &known.shapes);

            for shape in &mut shapes {
                let crate::declarations::Shape::Struct { name, fields, .. } = shape else {
                    continue;
                };
                let Some((_, names)) = private_methods.iter().find(|(t, _)| t == name) else {
                    continue;
                };

                for n in names {
                    if !fields.iter().any(|(f, _)| f == n) {
                        fields.push((n.clone(), true));
                    }
                }
            }

            (f.rel.clone(), shapes)
        })
        .collect();

    for d in &mut analysis.diagnostics {
        if d.kind != "TypeError" && d.kind != "SyntaxError" {
            continue;
        }

        let whole = files
            .iter()
            .find(|f| f.rel == d.rel)
            .map(|f| f.source.as_str());
        let source = whole.and_then(|s| s.lines().nth(d.line.saturating_sub(1)));
        let shapes = per_file
            .iter()
            .find(|(rel, _)| *rel == d.rel)
            .map_or(known.shapes.as_slice(), |(_, s)| s.as_slice());
        // Two enums of one variant set print alike; the fold names the
        // first it knows, so the file's own come first.
        let reach = crate::shapes::Known {
            shapes: shapes.to_vec(),
            interfaces: known.interfaces.clone(),
            namespaces: known.namespaces.clone(),
            tables: known.tables.clone(),
        };
        d.message = friendly_type_message(&d.message, &reach, source, d.col);

        // `new Nope { }` names a struct, not a global, and the report
        // moves onto the name.
        if let Some(text) = whole
            && let Some(better) = unknown_struct_report(
                &d.message,
                &root.join(&config.build.input).join(&d.rel),
                text,
                d.line,
            )
        {
            d.kind = better.kind.to_string();
            d.message = better.message;

            if let Some((line, col)) = better.at {
                d.line = line;
                d.col = col;
            }

            continue;
        }

        if let Some(text) = whole
            && let Some(message) = crate::modules::missing_import_message(
                &d.message,
                &root.join(&config.build.input).join(&d.rel),
                text,
            )
        {
            d.message = message;

            continue;
        }

        if let Some(text) = whole
            && let Some((message, at)) = rewrite_emitted_name(&d.message, text, d.line)
        {
            d.message = message;

            if let Some(at) = at {
                d.line = at;
                d.col = 1;
            }

            continue;
        }

        if let Some(text) = whole
            && let Some(better) = resite_report(&d.message, shapes, text, d.line, d.col)
        {
            d.kind = better.kind.to_string();
            d.message = better.message;

            if let Some((line, col)) = better.at {
                d.line = line;
                d.col = col;
            }
        }
    }

    // A report the resite moved onto a line the compiler already
    // reported says the same thing twice: a duplicate declaration lands
    // on the second name, where Alloy's own report stands.
    analysis.diagnostics.retain(|d| {
        !(d.kind == "TypeError" || d.kind == "SyntaxError")
            || !files
                .iter()
                .any(|f| f.rel == d.rel && f.error_lines.contains(&d.line))
    });

    // The checker checks an invariant position both ways and against
    // the optional a method's parameter carries, so one mistake reads
    // three times at one place. Nothing in the source is optional.
    let mut sites: Vec<(PathBuf, usize, usize, String)> = Vec::new();

    analysis.diagnostics.retain(|d| {
        let key = (
            d.rel.clone(),
            d.line,
            d.col,
            d.message.replace("?'", "'").replace("?`", "`"),
        );

        match sites.contains(&key) {
            true => false,

            false => {
                sites.push(key);

                true
            }
        }
    });

    // The checker reads a private member as missing, because the
    // struct's own file keeps it out of the public view. `alloy doc
    // private` promises a type error there, so the report that names
    // the member as private stands beside `private_access`. The other
    // two shapes of the same cause name no private member and would
    // send the reader after a member that is there, so they go.
    let reads_as_missing =
        |m: &str| m.contains("has no method") || m.contains("not found in table");

    analysis.diagnostics.retain(|d| {
        !reads_as_missing(&d.message)
            || !files.iter().any(|f| {
                f.rel == d.rel
                    && f.lint_lines
                        .iter()
                        .any(|(at, name)| *at == d.line && *name == "private_access")
            })
    });

    // Two method bodies of one `impl` report the same mistake once the
    // rewrite moves both to the `impl` line.
    let mut seen: Vec<(PathBuf, usize, usize, String)> = Vec::new();

    analysis.diagnostics.retain(|d| {
        let key = (d.rel.clone(), d.line, d.col, d.message.clone());

        match seen.contains(&key) {
            true => false,

            false => {
                seen.push(key);

                true
            }
        }
    });

    // A nil base makes every key on it unknown. `could be nil` names
    // the problem; the key report sends the reader after a typo that
    // is not there.
    let nil_lines: Vec<(PathBuf, usize)> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("could be nil"))
        .map(|d| (d.rel.clone(), d.line))
        .collect();

    analysis.diagnostics.retain(|d| {
        !(d.message.starts_with("Key '") && nil_lines.contains(&(d.rel.clone(), d.line)))
    });

    drop_nil_echo(&mut analysis.diagnostics);

    // The checker gave up on the line: what else it says there comes
    // from a solve it did not finish.
    let limit_lines: Vec<(PathBuf, usize)> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.message.contains(SOLVER_LIMIT))
        .map(|d| (d.rel.clone(), d.line))
        .collect();

    analysis.diagnostics.retain(|d| {
        d.message.contains(SOLVER_LIMIT)
            || d.kind != "TypeError"
            || !limit_lines.contains(&(d.rel.clone(), d.line))
    });

    // A `.` where a `:` belongs draws the arity error and then every
    // mismatch that follows from the shifted arguments. The one
    // sentence that names the mistake stands alone.
    let typo_lines: Vec<(PathBuf, usize)> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.message.contains(DOT_FOR_COLON))
        .map(|d| (d.rel.clone(), d.line))
        .collect();

    analysis.diagnostics.retain(|d| {
        d.message.contains(DOT_FOR_COLON) || !typo_lines.contains(&(d.rel.clone(), d.line))
    });

    analysis
        .diagnostics
        .sort_by(|a, b| (&a.rel, a.line, a.col).cmp(&(&b.rel, b.line, b.col)));
    analysis.diagnostics.dedup();
    keep_innermost(&mut analysis.diagnostics);

    Ok(analysis)
}

/// The shapes one file reaches, nearest first: the ones it declares,
/// then the ones its text names, as an import does, then the rest of
/// the project. Two enums with one variant set print alike, and a fold
/// names the first it meets.
fn shapes_in_reach(
    source: &str,
    all: &[crate::declarations::Shape],
) -> Vec<crate::declarations::Shape> {
    let mut shapes = crate::declarations::shapes(source);
    let names = |s: &crate::declarations::Shape| !shapes.iter().any(|h| h.name() == s.name());
    let mut rest: Vec<_> = all.iter().filter(|s| names(s)).cloned().collect();
    let words: Vec<&str> = source
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .collect();
    let mentioned = |s: &crate::declarations::Shape| {
        let last = s.name().rsplit('.').next().unwrap_or(s.name());

        words.contains(&last)
    };
    rest.sort_by_key(|s| !mentioned(s));
    shapes.extend(rest);

    shapes
}

/// The names the fold may use: every struct, enum, mapped alias, and
/// interface the project declares.
pub fn known_shapes(files: &[CheckSource]) -> crate::shapes::Known {
    crate::shapes::Known {
        shapes: files
            .iter()
            .flat_map(|f| crate::declarations::shapes(&f.source))
            .collect(),
        interfaces: files
            .iter()
            .flat_map(|f| crate::shapes::interfaces(&f.source))
            .collect(),
        namespaces: files
            .iter()
            .flat_map(|f| crate::declarations::namespace_names(&f.source))
            .collect(),
        tables: files
            .iter()
            .flat_map(|f| crate::tables::plain_tables(&f.source))
            .collect(),
    }
}

/// Whether a report at an opening `(` of the artifact repeats a report
/// at one of `inner`, the positions of the same message, inside that
/// bracket. Positions are one-based.
fn repeats_an_inner_report(
    check: &str,
    line: usize,
    col: usize,
    inner: impl Iterator<Item = (usize, usize)>,
) -> bool {
    let offset = |line: usize, col: usize| {
        let start = check
            .split_inclusive('\n')
            .take(line.saturating_sub(1))
            .map(str::len)
            .sum::<usize>();

        start + col.saturating_sub(1)
    };
    let open = offset(line, col);

    if check.as_bytes().get(open) != Some(&b'(') {
        return false;
    }

    // The matching `)`, past any bracket inside a string.
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut close = None;

    for (i, &b) in check.as_bytes().iter().enumerate().skip(open) {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,

            (Some(_), _) => {}

            (None, b'"' | b'\'') => quote = Some(b),

            (None, b'(') => depth += 1,

            (None, b')') => {
                depth -= 1;

                if depth == 0 {
                    close = Some(i);

                    break;
                }
            }

            _ => {}
        }
    }

    let Some(close) = close else {
        return false;
    };

    inner.into_iter().any(|(l, c)| {
        let at = offset(l, c);

        at > open && at < close
    })
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
    if let Some(d) = definitions_line(line) {
        return Some(d);
    }

    // With a sourcemap the checker writes the file's place in the tree
    // after its path: `/m/build/a.luau [game/ReplicatedStorage/a](3,12)`.
    // A name in the tree may hold a `(`, so the bracket goes first.
    let first = line.find('(')?;
    let placed = line[..first]
        .find(" [")
        .and_then(|b| Some((b, b + line[b..].find("](")? + 1)));
    let (path, open) = match placed {
        Some((end, open)) => (&line[..end], open),

        None => (&line[..first], first),
    };
    let close = line[open..].find(')')? + open;
    let (l, c) = line[open + 1..close].split_once(',')?;
    let rest = line[close + 1..].strip_prefix(": ")?;
    let (kind, message) = rest.split_once(": ")?;

    if !kind.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }

    Some(Line {
        path,
        line: l.trim().parse().ok()?,
        col: c.trim().parse().ok()?,
        kind,
        message,
    })
}

/// A report about a definitions file. The checker names the file and
/// nothing else, as `<path>: Kind: message`, so the position comes back
/// zero and the caller finds the place in the artifact.
fn definitions_line(line: &str) -> Option<Line<'_>> {
    let (path, rest) = line.split_once(": ")?;

    if !path.ends_with(".d.luau") {
        return None;
    }

    let (kind, message) = rest.split_once(": ")?;

    if !kind.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }

    Some(Line {
        path,
        line: 0,
        col: 0,
        kind,
        message,
    })
}

/// The name of a definitions file the `.d.aly` files merge into, under
/// the mirror, before its number: `.alloy-declared-0.d.luau`. The copy
/// that carries the extensions keeps the name after `ext-`.
const DECLARED: &str = ".alloy-declared-";

/// Whether the compiler already reports an unknown type of a
/// definitions file: a type of the Alloy std, which the file cannot
/// reach. Any other unknown type drops the whole file, so its report
/// stands.
fn alloy_reports_the_type(name: &str) -> bool {
    crate::desugar::AMBIENT_TYPES.contains(&name)
}

/// The place a report with no position names: the one-based line of the
/// artifact that writes the name the message quotes, and its column.
fn quoted_place(text: &str, message: &str) -> Option<(usize, usize)> {
    text.lines()
        .enumerate()
        .find_map(|(i, text)| named_column(text, message).map(|col| (i + 1, col)))
}

/// The source position of an output position: the same line, the
/// column through the map. `None` drops the report: a silenced line, or
/// a warning about generated text.
fn map_position(
    f: &CheckSource,
    line: usize,
    col: usize,
    is_error: bool,
    kind: &str,
    message: &str,
) -> Option<(usize, usize)> {
    let silence = crate::directives::scan(&f.source);

    // The checker's kind is the name an `--@alloy-ignore-start` may
    // carry, so a region for `LocalUnused` silences that alone.
    if !silence.allows_named(line.saturating_sub(1), Some(kind)) {
        return None;
    }

    let out_off = offset_of(&f.check, line, col)?;

    if !is_error && f.map.is_generated(out_off as u32) {
        // A deprecated component reads by its tag, `<OldRow />`. The
        // lowering writes the call, and the name stays on the line.
        if kind != "DeprecatedApi" {
            return None;
        }

        let col = f
            .source
            .lines()
            .nth(line.saturating_sub(1))
            .and_then(|text| named_column(text, message))?;

        return Some((line, col));
    }

    // `$nameof(x)` and `$stringify(x)` turn their argument into a
    // string, so the checker sees no use of `x` where the source has one.
    if let Some(name) = unused_name(message)
        && consumed_by_intrinsic(&f.source, name)
    {
        return None;
    }

    let src_off = f.map.to_source(out_off as u32) as usize;
    let (sl, sc) = crate::directives::line_col(&f.source, src_off);

    if sl == line {
        return Some((line, sc));
    }

    // The markup lowering moves an expression off the line it was
    // written on, so the map answers with the tag's place instead. The
    // name the report quotes is still on the line the reader reads.
    let col = f
        .source
        .lines()
        .nth(line.saturating_sub(1))
        .and_then(|text| named_column(text, message))
        .unwrap_or(1);

    Some((line, col))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The checker writes a match arm's mistake at the arm and again at
    /// the `(` of the whole match. Only the outer copy goes.
    #[test]
    fn a_report_at_a_bracket_yields_to_the_same_report_inside() {
        let check = "local x = (\n    if a then b + \")\" else 0\n)\nprint(x + 1)\n";

        assert!(repeats_an_inner_report(check, 1, 11, [(2, 15)].into_iter()));
        // A report outside the bracket is another mistake.
        assert!(!repeats_an_inner_report(check, 1, 11, [(4, 7)].into_iter()));
        // A report that is not at a bracket stays.
        assert!(!repeats_an_inner_report(
            check,
            2,
            15,
            [(2, 15)].into_iter()
        ));
    }

    #[test]
    fn the_analyzer_line_parses() {
        let d = parse_line("src/a.luau(3,12): TypeError: Expected 'number', got 'string'").unwrap();
        assert_eq!(d.path, "src/a.luau");
        assert_eq!((d.line, d.col), (3, 12));
        assert_eq!(d.kind, "TypeError");
        assert!(d.message.starts_with("Expected"));
        assert!(parse_line("[INFO] Loading definitions file").is_none());

        // A report about a definitions file carries no position; the
        // name the message quotes says where it belongs.
        let d = parse_line("/m/build/junk.d.luau: TypeError: Unknown type 'Nope'").unwrap();
        assert_eq!(d.path, "/m/build/junk.d.luau");
        assert_eq!((d.line, d.col), (0, 0));
        assert_eq!(d.kind, "TypeError");
        assert!(parse_line("[INFO] Loading definitions file: @roblox - a.d.luau").is_none());

        // A file the sourcemap places carries its place in the tree.
        let d = parse_line(
            "/m/build/a.luau [game/ReplicatedStorage/Model (1)/a](3,12): TypeError: Expected 'number'",
        )
        .unwrap();
        assert_eq!(d.path, "/m/build/a.luau");
        assert_eq!((d.line, d.col), (3, 12));
        assert_eq!(d.message, "Expected 'number'");
    }

    /// Two enums with one variant set print alike. The file imports
    /// `Opt`, so `Opt` stands before `Ns.Opt2` for the fold, whatever
    /// the order of the project's files.
    #[test]
    fn the_shapes_a_file_names_come_first() {
        let variants = vec![
            ("Some".to_string(), vec!["T".to_string()]),
            ("Nil".to_string(), vec![]),
        ];
        let all = vec![
            crate::declarations::Shape::Enum {
                name: "Ns.Opt2".into(),
                generics: vec!["T".into()],
                variants: variants.clone(),
            },
            crate::declarations::Shape::Enum {
                name: "Opt".into(),
                generics: vec!["T".into()],
                variants,
            },
        ];
        let source = "import { Opt } from \"./opt\"\nlocal a = Opt.Some(1)\n";
        let reach = shapes_in_reach(source, &all);
        let names: Vec<&str> = reach.iter().map(|s| s.name()).collect();

        assert_eq!(names, ["Opt", "Ns.Opt2"]);
    }

    /// A `.d.aly` reaches the checker as definitions, and a report on
    /// one of those carries no position. The name the message quotes
    /// says where it belongs, and the names Alloy knows are types stay
    /// out: the checker reads a definitions file on its own.
    #[test]
    fn a_definitions_report_lands_on_the_name_it_quotes() {
        let src = "declare function f(v: Nope): ()\n";
        let out = crate::compile_with(
            src,
            &crate::EmitOptions {
                check: true,
                file_name: "junk.d.aly".to_string(),
                ..crate::EmitOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            quoted_place(&out.check, "Unknown type 'Nope'"),
            Some((1, 23))
        );

        // The compiler reports a std type in a definitions file. A
        // sibling's type sits in the same merged file, so a report of
        // one stands.
        assert!(alloy_reports_the_type("Result"));
        assert!(!alloy_reports_the_type("Kind"));
        assert!(!alloy_reports_the_type("Nope"));
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
            lint_lines: Vec::new(),
            error_lines: Vec::new(),
            parsed_clean: true,
            expected_hits: Vec::new(),
        };
        assert_eq!(
            map_position(&f, 1, 19, true, "TypeError", "Expected"),
            Some((1, 19))
        );
        let silenced = CheckSource {
            source: "local x: number = \"s\" --@alloy-ignore\n".to_string(),
            ..f
        };
        assert_eq!(
            map_position(&silenced, 1, 19, true, "TypeError", "Expected"),
            None
        );
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
