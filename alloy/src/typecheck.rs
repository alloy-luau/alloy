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
        match self.kind.as_str() {
            "UnknownModule" => Some("3.2"),
            "DirectiveError" => Some("4.4"),
            _ => None,
        }
    }

    /// A type or syntax error, as opposed to one of the checker's lints.
    pub fn is_error(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "TypeError" | "SyntaxError" | "UnknownModule" | "DirectiveError"
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
        let skip = [".git", "target", "node_modules", ".luaurc", ".config.luau"]
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
    // names a folder under `in`, a mount, points at its output here.
    let mut luau = crate::luau_config::read_dir(root)
        .map(|(_, c)| c)
        .unwrap_or_default();

    if luau.language_mode.is_none() {
        luau.language_mode = Some("strict".to_string());
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

    for d in &config.flux.definitions {
        definitions.push(root.join(d));
    }

    // A plain `.luau` beside the sources sits in the output too, as the
    // build copies it, so a require of it resolves.
    let input = root.join(&config.build.input);
    let mut plain = Vec::new();
    let _ = crate::build::walk_plain(&input, &mut plain);

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
    let _ = crate::build::walk_data(&input, &mut data);

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
        .arg("--flag:LuauSolverV2=true")
        // A printed type must arrive whole: `friendly_type_message`
        // folds an emitted table back to the name the source wrote, and
        // the default limit cuts it to `*TRUNCATED*` first. The language
        // server raises the same two flags, so both say one thing.
        .arg("--flag:LuauTypeMaximumStringifierLength=200000")
        .arg("--flag:LuauTableTypeMaximumStringifierLength=200000");

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

    let known = known_shapes(files);
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
        let Ok(rel_out) = path.strip_prefix(&config.build.out) else {
            continue;
        };
        let Some(f) = files
            .iter()
            .find(|f| crate::build::output_for(&f.rel).as_deref() == Some(rel_out))
        else {
            continue;
        };

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
            // No quoted path on the line means `require(script.Parent)`
            // or another runtime path. It resolves in Roblox, and the
            // `raw_require` lint already says the checker cannot follow
            // it, so there is nothing to report here.
            let Some(spec) = quoted_on_line(&f.source, mapped.0.saturating_sub(1)) else {
                continue;
            };
            let rel = config.build.input.join(&f.rel);

            (
                "UnknownModule".to_string(),
                unknown_module_message(&spec, &rel),
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

    for d in &mut analysis.diagnostics {
        if d.kind != "TypeError" && d.kind != "SyntaxError" {
            continue;
        }

        let whole = files
            .iter()
            .find(|f| f.rel == d.rel)
            .map(|f| f.source.as_str());
        let source = whole.and_then(|s| s.lines().nth(d.line.saturating_sub(1)));
        d.message = friendly_type_message(&d.message, &known, source, d.col);

        if let Some(text) = whole
            && let Some((message, at)) = rewrite_emitted_name(&d.message, text, d.line)
        {
            d.message = message;

            if let Some(at) = at {
                d.line = at;
                d.col = 1;
            }
        }
    }

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
    }
}

/// The runtime's own names taken out of a printed type: the require
/// binding, the mapped-type functions, and the `__all` suffix an
/// exported table carries.
fn strip_std_prefix(text: &str) -> String {
    if !(text.contains("__alloy") || text.contains("__mapped_") || text.contains("__all")) {
        return text.to_string();
    }

    let mut out = text.to_string();

    for primitive in crate::desugar::PRIMITIVES {
        out = out.replace(&format!("__alloy_{primitive}."), &format!("{primitive}."));
    }

    out.replace("__alloy.", "")
        .replace("__all", "")
        .replace("__mapped_optional<", "Partial<")
        .replace("__mapped_read<", "Readonly<")
        .replace("__mapped_write<", "Sink<")
}

/// A checker message as a reader of the source should see it: the
/// runtime's names go, a struct's private view folds to the struct,
/// `Array<T>` reads `T[]`, and the tail that walks the emitted shape is
/// cut. The language server runs the same pass, so the terminal and the
/// editor say one thing.
pub fn friendly_type_message(
    message: &str,
    known: &crate::shapes::Known,
    line: Option<&str>,
    col: usize,
) -> String {
    // The shared pass writes a kind of its own; the caller prints the
    // report's kind, so one of the two goes.
    let cut = crate::shapes::friendly_text(message);
    let cut = cut
        .split_once(": ")
        .filter(|(kind, _)| kind.ends_with("Error") && !kind.contains(' '))
        .map_or(cut.as_str(), |(_, rest)| rest);
    let stripped = strip_std_prefix(cut);
    let folded = drop_result_methods(&crate::shapes::fold(&stripped, known));

    if let Some(hint) = crate::shapes::plain_table_hint(&folded) {
        return hint;
    }

    let Some(line) = line else {
        return folded;
    };

    // The remote rewrite reads the surface the checker printed, so it
    // runs on the text before the fold names it as well as after.
    if let Some(better) = rewrite_remote_key(&stripped, line)
        .or_else(|| rewrite_remote_key(&folded, line))
        .or_else(|| rewrite_await(&folded, line))
        .or_else(|| rewrite_arity(&folded, line, col))
        .or_else(|| rewrite_dot_self(&folded, line, col))
    {
        return better;
    }

    match constructor_field(line, col) {
        Some((field, name)) => format!("field `{field}` of `{name}`: {folded}"),

        None => folded,
    }
}

/// The one sentence for a `.` where a `:` belongs, and for the arity of
/// a method call the source writes without `self`. The CLI and the
/// editor both call it, so both say the same thing.
///
/// `line` is the source line the report sits on. `col` is one-based.
pub fn rewrite_dot_call(message: &str, line: &str, col: usize) -> Option<String> {
    rewrite_arity(message, line, col).or_else(|| rewrite_dot_self(message, line, col))
}

/// The phrase that names a `.` where a `:` belongs. A report on a line
/// that already carries it is the same mistake told again.
pub const DOT_FOR_COLON: &str = "` is a method; call it with `";

/// The checker's lints Alloy replaces outright: `unused_variable`,
/// `unused_function`, and `unused_import` cover the same ground, in the
/// words of what the source wrote.
pub fn owned_lint(kind: &str) -> bool {
    matches!(kind, "LocalUnused" | "FunctionUnused" | "ImportUnused")
}

/// The Alloy lints that say what one of the checker's lints says. Alloy
/// names the construct the source wrote, so where both fire on a line
/// the checker's copy goes.
pub fn paired_lint(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "LocalUnused" => &["unused_variable"],

        "FunctionUnused" => &["unused_function"],

        "ImportUnused" => &["unused_import"],

        "TableLiteral" => &["duplicate_key"],

        "ComparisonPrecedence" => &["misplaced_not", "bool_comparison"],

        "DeprecatedApi" => &["deprecated_global", "deprecated_method"],

        "TableOperations" => &["table_insert_position", "manual_push"],

        _ => return None,
    })
}

/// A report about a name the emit writes and the source does not, in
/// the source's words: `new Plain { }` and `x is Plain` on a type
/// alias, `impl T for Alias`, and `new n { }` on a value. The second
/// half of the answer is the line to move the report to; the `impl`
/// case reports once on the `impl` line rather than once per method.
/// The language server writes the same sentences.
pub fn rewrite_emitted_name(
    message: &str,
    source: &str,
    line: usize,
) -> Option<(String, Option<usize>)> {
    let text = source.lines().nth(line.saturating_sub(1))?;

    if let Some(name) = quoted_after(message, "Unknown global '") {
        if names_word(text, &format!("new {name}")) {
            return Some((format!("`{name}` is a type, not a struct"), None));
        }

        if names_word(text, &format!("is {name}")) {
            return Some((format!("`{name}` is not a type in scope"), None));
        }

        // Every method body of `impl T for Alias` reports the same
        // global; the `impl` line is where the mistake is.
        if let Some(at) = impl_line_for(source, line, name) {
            return Some((
                format!("`{name}` is a type, not a struct; `impl` needs one"),
                Some(at),
            ));
        }

        // A `type`, an `interface`, or a `trait` binds no value, so the
        // emit passes the name through and the checker looks for a
        // global of that name.
        if declares_type_only(source, name) {
            return Some((format!("`{name}` is a type, not a value"), None));
        }
    }

    // `new n { }`, where `n` is a value: the emit asks it for `new`.
    if let Some(owner) = quoted_after(message, "Type '")
        && message.ends_with("does not have key 'new'")
        && let Some(name) = word_after(text, "new ")
    {
        return Some((format!("`new` needs a struct; `{name}` is a {owner}"), None));
    }

    None
}

/// The text between `opener` and the next quote.
fn quoted_after<'a>(message: &'a str, opener: &str) -> Option<&'a str> {
    let at = message.find(opener)? + opener.len();

    message[at..].find('\'').map(|end| &message[at..at + end])
}

/// The identifier right after `opener` on a line.
fn word_after(line: &str, opener: &str) -> Option<String> {
    let at = line.find(opener)? + opener.len();
    let name: String = line[at..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    (!name.is_empty()).then_some(name)
}

/// Whether the line holds the phrase as whole words.
fn names_word(line: &str, phrase: &str) -> bool {
    line.match_indices(phrase).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + phrase.len()..].chars().next();

        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Whether the source declares the name as a type alone: a `type`, an
/// `interface`, or a `trait`. A struct and an enum bind a value too.
fn declares_type_only(source: &str, name: &str) -> bool {
    source.lines().any(|l| {
        let text = l.trim_start();
        let text = text.strip_prefix("export ").unwrap_or(text);

        ["type ", "interface ", "trait "].iter().any(|head| {
            text.strip_prefix(head).is_some_and(|rest| {
                rest.strip_prefix(name).is_some_and(|tail| {
                    !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
            })
        })
    })
}

/// The one-based line of the `impl ... for Name` above a line, when one
/// opens the block the line sits in.
fn impl_line_for(source: &str, line: usize, name: &str) -> Option<usize> {
    source
        .lines()
        .take(line.saturating_sub(1))
        .enumerate()
        .filter(|(_, l)| {
            l.trim_start().starts_with("impl ") && l.trim_end().ends_with(&format!(" for {name}"))
        })
        .map(|(k, _)| k + 1)
        .last()
}

/// The std holds a Result's methods in an alias of their own, so the
/// checker prints `ResultMethods<T, E> & Result<T, E>`. The methods are
/// part of what `Result` is; the name for them is not the reader's.
fn drop_result_methods(text: &str) -> String {
    let mut out = text.to_string();

    while let Some(at) = out.find("ResultMethods") {
        let rest = &out[at..];
        let Some(open) = rest.find('<') else {
            break;
        };
        let Some(close) = rest[open..].find("> & ").map(|i| open + i + "> & ".len()) else {
            break;
        };

        out.replace_range(at..at + close, "");
    }

    out
}

/// The call that starts at a column: `:` or `.`, the receiver as the
/// source writes it, and the name after the separator.
fn call_head(line: &str, col: usize) -> Option<(char, String, String)> {
    let start = col.saturating_sub(1);
    let rest = line.get(start..)?;
    let head = &rest[..rest.find('(')?];
    let sep = head.rfind([':', '.'])?;
    let member = head[sep + 1..].trim();
    let name = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');

    if !name(member) {
        return None;
    }

    let receiver = head[..sep].trim();

    (!receiver.is_empty()).then(|| {
        (
            head.as_bytes()[sep] as char,
            receiver.to_string(),
            member.to_string(),
        )
    })
}

/// The counts of an argument-count message: what the function takes and
/// what the call passed. A range, `1 to 2`, gives its lower bound.
fn arity_counts(message: &str) -> Option<(usize, usize)> {
    let after = message.split_once("expects ")?.1;
    let expects: usize = after
        .split_whitespace()
        .next()?
        .parse()
        .ok()
        .filter(|n| *n > 0)?;
    let rest = message.split_once(", but ")?.1;
    let word = rest.trim_start_matches("only ").split_whitespace().next()?;
    let given = if word == "none" {
        0
    } else {
        word.parse().ok()?
    };

    Some((expects, given))
}

/// The argument-count message the source earns. A `:` call passes the
/// receiver as the first argument, which the reader did not write, so
/// both counts lose it. A `.` call of a method is one argument short
/// for that same reason, and that mistake reads better named.
fn rewrite_arity(message: &str, line: &str, col: usize) -> Option<String> {
    if !message.contains("Function expects") {
        return None;
    }

    let (expects, given) = arity_counts(message)?;
    let (sep, receiver, member) = call_head(line, col)?;

    if sep == '.' {
        // `expects 1 to 2 arguments` is a range: the call is short of
        // the lower bound, which says nothing about the separator.
        let ranged = message.contains(" to ");
        // A capitalized receiver names a module, a type, or a remote,
        // and each of those takes its `.`.
        let value = receiver.starts_with(|c: char| c.is_lowercase() || c == '_');

        return (!ranged && value && given + 1 == expects).then(|| {
            format!(
                "`{member}` is a method; call it with `{receiver}:{member}(...)`, not `{receiver}.{member}(...)`"
            )
        });
    }

    if given == 0 {
        return None;
    }

    let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
    let (expects, given) = (expects - 1, given - 1);
    let tail = if given < expects {
        format!(
            "but only {given} {} specified",
            if given == 1 { "is" } else { "are" }
        )
    } else {
        format!(
            "but {given} {} specified",
            if given == 1 { "is" } else { "are" }
        )
    };

    Some(format!(
        "Argument count mismatch. `{member}` takes {expects} {}, {tail}",
        plural(expects)
    ))
}

/// A `.` call of a method sends the first argument where the receiver
/// belongs, so the checker reports the mismatch against the method's
/// self parameter. The std writes that parameter `read T`, a type the
/// source never spells; the separator is the mistake, and it reads as
/// the arity rewrite says it.
fn rewrite_dot_self(message: &str, line: &str, col: usize) -> Option<String> {
    if !message.starts_with("Expected this to be 'read ") {
        return None;
    }

    let (sep, receiver, member) = enclosing_call(line, col)?;

    // A capitalized receiver names a module, a type, or a remote, and
    // each of those takes its `.`.
    (sep == '.' && receiver.starts_with(|c: char| c.is_lowercase() || c == '_')).then(|| {
        format!(
            "`{member}` is a method; call it with `{receiver}:{member}(...)`, not `{receiver}.{member}(...)`"
        )
    })
}

/// The call whose arguments hold a column: the separator, the receiver,
/// and the member. `call_head` reads a call that starts at the column;
/// this one reads the call the column sits inside.
fn enclosing_call(line: &str, col: usize) -> Option<(char, String, String)> {
    let upto = line.get(..col.saturating_sub(1))?;
    let open = upto.rfind('(')?;
    let head = &upto[..open];
    let sep = head.rfind([':', '.'])?;
    let member = head[sep + 1..].trim();
    let name = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');

    if !name(member) {
        return None;
    }

    let receiver: String = head[..sep]
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();

    (!receiver.is_empty()).then(|| (head.as_bytes()[sep] as char, receiver, member.to_string()))
}

/// `await` on a value that is no Future prints the std's own parameter,
/// `Awaitable<T>`, whose `T` is bound to nothing the reader can see.
fn rewrite_await(message: &str, line: &str) -> Option<String> {
    let wanted = message.contains("'Awaitable<T>'") || message.contains("'Future<T>'");

    if !(wanted && line.contains("await ")) {
        return None;
    }

    let got = message.split_once("but got '")?.1;
    let got = got.split('\'').next()?;
    // A narrowed primitive prints as `typeof(string)`; the reader wrote
    // a string.
    let got = got
        .strip_prefix("typeof(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(got);

    Some(format!("`await` needs a Future; `{got}` is not one"))
}

/// A remote's whole surface reaches a missing-member message. The
/// reader knows it by the name they declared.
fn rewrite_remote_key(message: &str, line: &str) -> Option<String> {
    let key = message.strip_prefix("Key '")?.split('\'').next()?;
    let table = message.split_once("' not found in table '")?.1;
    // Every side of a remote carries `instance` and at least one of the
    // verbs; the fold may have named the whole surface already.
    let surface = table.contains("instance: Instance?")
        && ["on:", "fire", "call:", "wait:"]
            .iter()
            .any(|verb| table.contains(verb));

    if !(table.starts_with("Remote'") || surface) {
        return None;
    }

    let at = line.find(&format!(".{key}"))?;
    let receiver: String = line[..at]
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if receiver.is_empty() {
        return None;
    }

    let members: Vec<&str> = table
        .trim_start_matches('{')
        .split(',')
        .filter_map(|part| part.split_once(':').map(|(k, _)| k.trim()))
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .collect();
    let near = members
        .iter()
        .map(|m| (edit_distance(m, key), *m))
        .filter(|(d, _)| *d <= 2)
        .min();

    Some(match near {
        Some((_, m)) => format!("remote `{receiver}` has no `{key}`; did you mean `{m}`?"),

        None => format!("remote `{receiver}` has no `{key}`"),
    })
}

/// The edit distance of two names, for a "did you mean".
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }

    row[b.len()]
}

/// The constructor field a column falls in: `new Plain { a = "x" }` at
/// the column of `"x"` gives `("a", "Plain")`. The checker reports the
/// value alone, and the reader wants to know which field it was for.
fn constructor_field(line: &str, col: usize) -> Option<(String, String)> {
    let at = line.find("new ")?;
    let rest = &line[at + 4..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if name.is_empty() {
        return None;
    }

    let open = at + 4 + rest.find('{')?;
    let target = col.checked_sub(1)?;

    if target <= open {
        return None;
    }

    let mut depth = 0i32;
    let mut field: Option<String> = None;
    let mut key_start = open + 1;

    for (i, c) in line.char_indices().skip(open) {
        match c {
            '{' | '(' | '[' => depth += 1,

            '}' | ')' | ']' => {
                depth -= 1;

                if depth == 0 {
                    break;
                }
            }

            ',' if depth == 1 => key_start = i + 1,

            '=' if depth == 1 => {
                let key = line[key_start..i].trim();

                if key.chars().all(|c| c.is_alphanumeric() || c == '_') && !key.is_empty() {
                    field = Some(key.to_string());
                }
            }

            _ => {}
        }

        if i == target {
            return field.map(|f| (f, name));
        }
    }

    None
}

/// One mistake reaches the checker through several nested ranges, so
/// one sentence lands on a line as many times as there are ranges. The
/// innermost is the one that points at the mistake, and on a line it
/// starts last, so the greatest column of a repeated sentence wins.
fn keep_innermost(diagnostics: &mut Vec<TypeDiag>) {
    let mut best: HashMap<(PathBuf, usize, String), usize> = HashMap::new();

    for d in diagnostics.iter() {
        let key = (d.rel.clone(), d.line, d.message.clone());
        let col = best.entry(key).or_insert(d.col);
        *col = (*col).max(d.col);
    }

    let mut seen: HashSet<(PathBuf, usize, String)> = HashSet::new();

    diagnostics.retain(|d| {
        let key = (d.rel.clone(), d.line, d.message.clone());

        if best.get(&key) != Some(&d.col) {
            return false;
        }

        seen.insert(key)
    });
}

/// The `UnknownModule` report for a require the checker could not
/// resolve, from the module path the source wrote and the source's own
/// path relative to the root: what was asked for, and where it was
/// looked for. The kind is the caller's prefix.
pub fn unknown_module_message(spec: &str, source_rel: &Path) -> String {
    if let Some(rest) = spec.strip_prefix('@') {
        let alias = rest.split('/').next().unwrap_or(rest);

        return format!(
            "\"{spec}\" names no module; no alias @{alias} in .luaurc or in the [mount] table"
        );
    }

    let base = source_rel.parent().unwrap_or(Path::new(""));
    let mut target = PathBuf::new();

    for c in base.join(spec).components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                if !target.pop() {
                    target.push("..");
                }
            }

            other => target.push(other),
        }
    }

    // A data path names one file; a module path names one of several.
    let what = match crate::data::Format::of(spec) {
        Some(format) => format!("no {} file", format.name()),

        None => "no .aly, .alx, or .luau file".to_string(),
    };

    format!(
        "\"{spec}\" names no module; {what} at {}",
        target.to_string_lossy().replace('\\', "/")
    )
}

/// The content of the first quoted string on a zero-based line.
pub fn quoted_on_line(source: &str, line: usize) -> Option<String> {
    let text = source.lines().nth(line)?;
    let open = text.find(['"', '\''])?;
    let quote = text.as_bytes()[open] as char;
    let rest = &text[open + 1..];
    let close = rest.find(quote)?;

    Some(rest[..close].to_string())
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
    fn an_unknown_module_names_what_was_asked_for() {
        assert_eq!(
            unknown_module_message("./ui", Path::new("src/app/main.aly")),
            "\"./ui\" names no module; no .aly, .alx, or .luau file at src/app/ui"
        );
        assert_eq!(
            unknown_module_message("../shared/util", Path::new("src/app/main.aly")),
            "\"../shared/util\" names no module; no .aly, .alx, or .luau file at src/shared/util"
        );
        assert_eq!(
            unknown_module_message("@packages/react", Path::new("src/main.aly")),
            "\"@packages/react\" names no module; no alias @packages in .luaurc or in the [mount] table"
        );
        assert_eq!(
            unknown_module_message("./data.json", Path::new("src/app/main.aly")),
            "\"./data.json\" names no module; no JSON file at src/app/data.json"
        );
        assert_eq!(
            quoted_on_line("import { a } from \"./x\"\nlocal y = 1\n", 0),
            Some("./x".to_string())
        );
    }

    #[test]
    fn await_on_a_plain_value_names_that_value() {
        let known = crate::shapes::Known::default();
        assert_eq!(
            friendly_type_message(
                "Expected this to be 'Awaitable<T>', but got 'number'",
                &known,
                Some("local nope = await n"),
                14
            ),
            "`await` needs a Future; `number` is not one"
        );
        // A narrowed primitive prints as `typeof(string)`.
        assert_eq!(
            friendly_type_message(
                "Expected this to be 'Awaitable<T>', but got 'typeof(string)'",
                &known,
                Some("local nope = await s"),
                14
            ),
            "`await` needs a Future; `string` is not one"
        );
    }

    #[test]
    fn a_mapped_result_reads_as_a_result() {
        let known = crate::shapes::Known::default();
        let message = "Expected this to be 'number', but got 'ResultMethods2<number, string> & { read _1: number | string, read __err: string, read __ok: number, tag: \"Err\" | \"Ok\", read trace: string? }'";
        assert_eq!(
            friendly_type_message(message, &known, None, 0),
            "Expected this to be 'number', but got 'Result<number, string>'"
        );
    }

    #[test]
    fn a_method_call_message_leaves_out_self() {
        let known = crate::shapes::Known::default();
        let line = "local b = xs:len(1, 2)";
        assert_eq!(
            friendly_type_message(
                "Argument count mismatch. Function expects 1 argument, but 3 are specified",
                &known,
                Some(line),
                11
            ),
            "Argument count mismatch. `len` takes 0 arguments, but 2 are specified"
        );
        assert_eq!(
            friendly_type_message(
                "Argument count mismatch. Function expects 3 arguments, but only 2 are specified",
                &known,
                Some("local c = xs:reduce(f)"),
                11
            ),
            "Argument count mismatch. `reduce` takes 2 arguments, but only 1 is specified"
        );
    }

    #[test]
    fn a_dot_call_of_a_method_names_the_colon() {
        let known = crate::shapes::Known::default();
        assert_eq!(
            friendly_type_message(
                "Argument count mismatch. Function expects 1 argument, but none are specified",
                &known,
                Some("local a = c.bump()"),
                11
            ),
            "`bump` is a method; call it with `c:bump(...)`, not `c.bump(...)`"
        );
        // A range of counts says nothing about the separator.
        assert!(
            friendly_type_message(
                "Argument count mismatch. Function expects 1 to 2 arguments, but none are specified",
                &known,
                Some("Toast.fire_all()"),
                1
            )
            .contains("Function expects 1 to 2")
        );
    }

    #[test]
    fn a_remote_surface_reads_as_the_remote() {
        let known = crate::shapes::Known::default();
        let message = "Key 'blast' not found in table '{ call: (Player, string) -> Future<any>, fire: (Player, string) -> (), instance: Instance?, spec: any }'";
        assert_eq!(
            friendly_type_message(message, &known, Some("Toast.blast(\"x\")"), 1),
            "remote `Toast` has no `blast`"
        );
        assert_eq!(
            friendly_type_message(
                "Key 'fira' not found in table 'Remote'",
                &known,
                Some("Toast.fira(\"x\")"),
                1
            ),
            "remote `Toast` has no `fira`"
        );
    }

    #[test]
    fn a_dot_call_that_lands_on_the_self_parameter_names_the_colon() {
        assert_eq!(
            friendly_type_message(
                "TypeError: Expected this to be 'read number[]', but got 'number'",
                &crate::shapes::Known::default(),
                Some("local d = xs.push(4)"),
                19,
            ),
            "`push` is a method; call it with `xs:push(...)`, not `xs.push(...)`"
        );
    }

    /// The checker answers a `{ ... }` where an Array belongs with the
    /// nineteen methods the table lacks. The reader wrote the wrong
    /// bracket.
    #[test]
    fn a_table_literal_where_an_array_belongs_names_the_bracket() {
        let message = "Table type '{string}' not compatible with type 'string[]' because the former is missing fields 'find', 'filter', 'push', 'map'";
        assert_eq!(
            friendly_type_message(message, &crate::shapes::Known::default(), None, 1),
            "a `{ ... }` is a plain table, not a `string[]`; an Array literal is `[ ... ]`"
        );
        // A table where a table belongs keeps the checker's words.
        let other = "Table type '{string}' not compatible with type '{ x: number }' because the former is missing fields 'x'";
        assert!(
            friendly_type_message(other, &crate::shapes::Known::default(), None, 1)
                .contains("missing fields"),
        );
    }

    /// A nil base makes every key on it unknown. `could be nil` names
    /// the problem; the key report sends the reader after a typo that
    /// is not there.
    #[test]
    fn the_unused_lints_and_the_nil_cascade_belong_to_alloy() {
        assert!(owned_lint("LocalUnused"));
        assert!(owned_lint("FunctionUnused"));
        assert!(owned_lint("ImportUnused"));
        assert!(!owned_lint("DeprecatedApi"));
    }

    /// The checker names the two emitted files of an import cycle;
    /// `circular_import` names the two the author wrote.
    #[test]
    fn the_emit_only_reports_are_dropped() {
        assert!(crate::shapes::names_only_the_emit(
            "TypeError: Key '%error-id%' not found in external type 'Player'"
        ));
        assert!(crate::shapes::names_only_the_emit(
            "Cyclic module dependency: /tmp/alloy-flux-1/root/build/a.luau -> /tmp/x/b.luau"
        ));
        assert!(!crate::shapes::names_only_the_emit(
            "Key 'Position' not found in external type 'Instance'"
        ));
    }

    #[test]
    fn a_checker_lint_pairs_with_the_alloy_one() {
        assert_eq!(paired_lint("TableLiteral"), Some(&["duplicate_key"][..]));
        assert_eq!(paired_lint("LocalUnused"), Some(&["unused_variable"][..]));
        assert_eq!(paired_lint("TypeError"), None);
    }

    #[test]
    fn a_new_on_a_type_alias_names_the_type() {
        let src = "type Plain = { a: number }\nlocal q = new Plain { a = 1 }\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Plain'; consider assigning to it first",
                src,
                2
            ),
            Some(("`Plain` is a type, not a struct".to_string(), None))
        );
    }

    #[test]
    fn an_is_test_against_no_type_says_so() {
        let src = "local v: any = 1\nif v is Nothing then print(\"?\") end\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Nothing'; consider assigning to it first",
                src,
                2
            ),
            Some(("`Nothing` is not a type in scope".to_string(), None))
        );
    }

    #[test]
    fn an_impl_for_an_alias_reports_on_the_impl_line() {
        let src = "type Alias = { z: number }\nimpl Shape for Alias\n    function area(self): number\n        return self.z\n    end\nend\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Alias'; consider assigning to it first",
                src,
                3
            ),
            Some((
                "`Alias` is a type, not a struct; `impl` needs one".to_string(),
                Some(2)
            ))
        );
    }

    #[test]
    fn a_type_printed_as_a_value_says_it_is_a_type() {
        let src = "type Alias3 = number\nprint(Alias3)\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Alias3'; consider assigning to it first",
                src,
                2
            ),
            Some(("`Alias3` is a type, not a value".to_string(), None))
        );

        let iface = "interface Both extends HasName as\n    id: number\nend\nprint(Both)\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Both'; consider assigning to it first",
                iface,
                4
            ),
            Some(("`Both` is a type, not a value".to_string(), None))
        );
    }

    #[test]
    fn a_new_on_a_value_names_what_it_holds() {
        let src = "local n = 5\nlocal r = new n {}\n";
        assert_eq!(
            rewrite_emitted_name("Type 'number' does not have key 'new'", src, 2),
            Some(("`new` needs a struct; `n` is a number".to_string(), None))
        );
    }

    #[test]
    fn a_constructor_message_names_the_field() {
        let known = crate::shapes::Known::default();
        let line = "local bad4 = new Plain { a = \"not a number\", b = \"x\" }";
        assert_eq!(
            friendly_type_message(
                "Expected this to be 'number', but got 'string'",
                &known,
                Some(line),
                30
            ),
            "field `a` of `Plain`: Expected this to be 'number', but got 'string'"
        );
        assert_eq!(
            constructor_field(line, 30),
            Some(("a".into(), "Plain".into()))
        );
        assert_eq!(
            constructor_field(line, 50),
            Some(("b".into(), "Plain".into()))
        );
        assert_eq!(constructor_field("local p = { a = 1 }", 13), None);
    }

    #[test]
    fn a_repeated_sentence_keeps_the_innermost_range() {
        let mut diagnostics = vec![
            TypeDiag {
                rel: PathBuf::from("a.aly"),
                line: 11,
                col: 12,
                kind: "TypeError".into(),
                message: "Operator '+' could not be applied".into(),
            },
            TypeDiag {
                rel: PathBuf::from("a.aly"),
                line: 11,
                col: 55,
                kind: "TypeError".into(),
                message: "Operator '+' could not be applied".into(),
            },
        ];
        keep_innermost(&mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].col, 55);
    }

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
