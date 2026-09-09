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

    // `alloy build` writes `sourcemap.json` at the root. A root that
    // still holds the `.alloy/sourcemap.json` an older build wrote uses
    // that one.
    let sourcemap = [
        root.join("sourcemap.json"),
        root.join(".alloy/sourcemap.json"),
    ]
    .into_iter()
    .find(|p| p.is_file());

    if let Some(sourcemap) = &sourcemap {
        cmd.arg("--sourcemap").arg(sourcemap);
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

            ("UnknownModule".to_string(), no_module_return_message(&spec))
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
    let per_file: Vec<(PathBuf, Vec<crate::declarations::Shape>)> = files
        .iter()
        .map(|f| {
            let mut shapes = crate::declarations::shapes(&f.source);
            let rest: Vec<_> = known
                .shapes
                .iter()
                .filter(|s| !shapes.iter().any(|h| h.name() == s.name()))
                .cloned()
                .collect();
            shapes.extend(rest);

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
        d.message = friendly_type_message(&d.message, &known, source, d.col);

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

        let shapes = per_file
            .iter()
            .find(|(rel, _)| *rel == d.rel)
            .map_or(known.shapes.as_slice(), |(_, s)| s.as_slice());

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

    // `private_access` already names the field and says who reaches it,
    // and it carries the line the source wrote. One report per mistake.
    analysis.diagnostics.retain(|d| {
        !d.message.contains("is private to")
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
        namespaces: files
            .iter()
            .flat_map(|f| crate::declarations::namespace_names(&f.source))
            .collect(),
    }
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
