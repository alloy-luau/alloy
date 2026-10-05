//! `alloy test`: one lest spec per source with a `@test`.
//!
//! A test lives beside the code it tests, so a source holds both. The
//! spec takes the `@test` functions and every top-level statement they
//! reach: the imports they use, the locals and functions they call, the
//! structs and impls those need. Nothing else of the module goes with
//! them, so the module's side effects stay out of the test VM. The
//! slice keeps the source's lines, so a failure points at the real one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Chunk, Expr, Import, ImportKind, ImportSpec, Stmt};
use alloy_syntax::lexer::{Tok, TokKind};

use crate::build::relative_require;
use crate::config::Config;
use crate::{Diagnostic, EmitOptions};

/// What one test build did.
#[derive(Debug, Default)]
pub struct Report {
    /// The specs written, relative to the root.
    pub written: Vec<PathBuf>,
    /// The specs that would change, under `--check`.
    pub stale: Vec<PathBuf>,
    /// Stale specs removed.
    pub removed: Vec<PathBuf>,
    /// The tests found, per source.
    pub tests: usize,
    pub diagnostics: Vec<(PathBuf, Diagnostic)>,
    pub failures: Vec<(PathBuf, String)>,
    pub notes: Vec<String>,
    /// What the run set up, such as the `@lest` alias.
    pub ok: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty() && self.failures.is_empty() && self.stale.is_empty()
    }
}

/// The folder under `[test] out` that holds the modules the specs
/// require: every source compiled with file paths, the runtime, the
/// data modules, and the plain files. The build output cannot serve:
/// under a mount its requires are instance paths.
pub const MODULES: &str = ".modules";

/// The modules folder, relative to the root.
pub fn modules_dir(config: &Config) -> PathBuf {
    config.test.out.join(MODULES)
}

/// The words a declaration may write in front of `function` or
/// `namespace`.
const LEAD_WORDS: &[&str] = &["export", "local", "global", "public", "private", "async"];

/// True when `line` writes the `@test` attribute.
fn writes_test_attr(line: &str) -> bool {
    let mut rest = line;

    while let Some(at) = rest.find("@test") {
        let before = rest[..at].chars().next_back();
        let after = rest[at + "@test".len()..].chars().next();
        let word = |c: char| c.is_alphanumeric() || c == '_';

        if !before.is_some_and(word) && !after.is_some_and(word) {
            return true;
        }

        rest = &rest[at + 1..];
    }

    false
}

/// True when the line declares a `function` or a `namespace`, past the
/// attributes and the words that may lead one.
fn opens_declaration(line: &str) -> bool {
    let mut words = line
        .split_whitespace()
        .skip_while(|w| LEAD_WORDS.contains(w) || w.starts_with('@'));

    matches!(words.next(), Some("function" | "namespace"))
}

/// The indent of a line, in characters.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/*
True when the byte at `at` sits inside a `@test` function or a `@test`
namespace. `$expect` needs one, and both the compiler and the editor
ask this, so they offer and accept the intrinsic in the same places.

The scan reads the lines above `at`. A line that starts further left
than every line below it opens the block around that byte, so the walk
keeps the smallest indent it has seen and looks only at the lines that
go under it. A `function` or a `namespace` found that way is a test
when `@test` sits on its line or on the lines right above it.
*/
pub fn encloses_test(src: &str, at: usize) -> bool {
    let head = &src[..at.min(src.len())];
    let lines: Vec<&str> = head
        .lines()
        .filter(|l| {
            let t = l.trim_start();

            !t.is_empty() && !t.starts_with("--")
        })
        .collect();
    let mut level = usize::MAX;
    let mut i = lines.len();

    while i > 0 {
        i -= 1;
        let indent = indent_of(lines[i]);

        if indent >= level {
            continue;
        }

        level = indent;

        if !opens_declaration(lines[i]) {
            continue;
        }

        if writes_test_attr(lines[i]) {
            return true;
        }

        // The attributes sit above the declaration, at its own indent.
        let mut j = i;

        while j > 0
            && indent_of(lines[j - 1]) == indent
            && lines[j - 1].trim_start().starts_with('@')
        {
            if writes_test_attr(lines[j - 1]) {
                return true;
            }

            j -= 1;
        }
    }

    false
}

/// The path of the spec for a source, relative to the test folder:
/// `a/b.aly` and `a/b.alx` become `a/b.spec.luau`. A source already
/// named for its tests, `b.spec.aly` or `b.test.aly`, keeps one `.spec`.
pub fn spec_for(rel: &Path) -> Option<PathBuf> {
    let name = rel.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".aly")
        .or_else(|| name.strip_suffix(".alx"))?;

    if stem.ends_with(".d") {
        return None;
    }

    let stem = stem
        .strip_suffix(".spec")
        .or_else(|| stem.strip_suffix(".test"))
        .unwrap_or(stem);

    Some(rel.with_file_name(format!("{stem}.spec.luau")))
}

/// What one top-level statement declares, and what it attaches to.
struct Decl {
    /// Names the statement binds at the top level.
    declares: Vec<String>,
    /// A name the statement extends: `impl X`, `function X.f`, `X.k = v`.
    /// The statement goes with the declaration of that name.
    attaches: Option<String>,
    /// A statement that declares nothing and runs for effect: a loop, a
    /// block, a call, an index assignment. It goes with the declarations
    /// it names, since a `for` that fills a table is what the table is.
    effect: bool,
    /// Every identifier inside the statement.
    refs: HashSet<String>,
    /// Whether the statement is a `@test` function.
    is_test: bool,
    /// The byte range.
    start: usize,
    end: usize,
}

fn name_of(src: &str, toks: &[Tok], span: alloy_syntax::ast::TokSpan) -> String {
    toks[span.start as usize].text(src).to_string()
}

/// The first name of an expression, for an assignment target.
fn head_name(src: &str, toks: &[Tok], e: &Expr) -> Option<(String, bool)> {
    let span = e.span();
    let first = toks[span.start as usize];

    if first.kind != TokKind::Ident {
        return None;
    }

    let plain = span.end == span.start + 1;

    Some((first.text(src).to_string(), plain))
}

/// Whether a namespace holds a `@test`, at any depth, or carries one
/// itself. Such a namespace is a test of the file: the emit puts each
/// member on the table, so the spec calls the test by its path.
fn namespace_has_test(src: &str, toks: &[Tok], ns: &alloy_syntax::ast::NamespaceDecl) -> bool {
    let tested = |attrs: &[alloy_syntax::ast::Attr]| {
        attrs.iter().any(|a| {
            a.name
                .is_some_and(|n| toks[n.start as usize].text(src) == "test")
        })
    };

    // `@test namespace Suite as ... end`: every public function of the
    // group is a test, so the whole group joins the spec.
    if tested(&ns.attributes) {
        return true;
    }

    ns.members.iter().any(|m| match &m.stmt {
        Stmt::Function(f) => tested(&f.attrs),
        Stmt::LocalFunction(f) => tested(&f.attrs),
        Stmt::Namespace(inner) => namespace_has_test(src, toks, inner),

        _ => false,
    })
}

/// What a file's names bind, for the uses a statement reads.
struct Names {
    /// The tokens that name a field, a method, a key or a variant.
    members: HashSet<usize>,
    /// The methods of each `impl` on a type the file does not declare,
    /// `impl string`. A test reaches one by its name after `:`, so that
    /// member name is a use of the impl.
    foreign_methods: HashSet<String>,
    /// Each binding with its declaring token and the token ranges its
    /// scope holds; `None` for a scope the walk does not know.
    bindings: Vec<crate::naming::ScopedBinding>,
}

fn describe(src: &str, toks: &[Tok], stmt: &Stmt, names: &Names) -> Decl {
    let span = stmt.span();
    let start = toks[span.start as usize].start as usize;
    let end = toks[(span.end as usize)
        .saturating_sub(1)
        .max(span.start as usize)]
    .end as usize;
    let mut declares = Vec::new();
    let mut attaches = None;
    let mut is_test = false;
    let has_test = |attrs: &[alloy_syntax::ast::Attr]| {
        attrs.iter().any(|a| {
            a.name
                .is_some_and(|n| toks[n.start as usize].text(src) == "test")
        })
    };

    // `export default function f` declares `f` as the plain declaration
    // does. The test artifact returns no export table, so the spec keeps
    // the function and drops the export (LANG_BUGS 131).
    match stmt.under_default() {
        Stmt::Local(l) => {
            for b in &l.names {
                match &b.destructure {
                    Some(_) => {
                        for j in b.name.start..b.name.end {
                            let t = toks[j as usize];

                            if t.kind == TokKind::Ident {
                                declares.push(t.text(src).to_string());
                            }
                        }
                    }

                    None => declares.push(name_of(src, toks, b.name)),
                }
            }
        }

        Stmt::PatternLocal(p) => {
            for j in p.span.start..p.span.end {
                let t = toks[j as usize];

                if t.kind == TokKind::Ident && toks[j as usize + 1].text(src) != "(" {
                    declares.push(t.text(src).to_string());
                }

                if t.text(src) == "=" {
                    break;
                }
            }
        }

        Stmt::LocalFunction(f) => {
            declares.push(name_of(src, toks, f.name));
            is_test = has_test(&f.attrs);
        }

        Stmt::Function(f) => {
            is_test = has_test(&f.attrs);

            match f.path.as_slice() {
                [only] => declares.push(name_of(src, toks, *only)),

                [head, ..] => attaches = Some(name_of(src, toks, *head)),

                [] => {}
            }
        }

        Stmt::Assign(a) => {
            if let [target] = a.targets.as_slice()
                && let Some((name, plain)) = head_name(src, toks, target)
            {
                if plain {
                    declares.push(name);
                } else {
                    attaches = Some(name);
                }
            }
        }

        Stmt::Import(i) => match &i.kind {
            ImportKind::Default(n) => declares.push(name_of(src, toks, *n)),

            ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                declares.push(name_of(src, toks, *n));

                for s in specs {
                    declares.push(name_of(src, toks, s.alias.unwrap_or(s.name)));
                }
            }

            ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                for s in specs {
                    declares.push(name_of(src, toks, s.alias.unwrap_or(s.name)));
                }
            }
        },

        Stmt::Namespace(ns) => {
            declares.push(name_of(src, toks, ns.name));
            is_test = namespace_has_test(src, toks, ns);
        }

        Stmt::Struct(s) => declares.push(name_of(src, toks, s.name)),
        Stmt::Enum(e) => declares.push(name_of(src, toks, e.name)),
        Stmt::Trait(t) => declares.push(name_of(src, toks, t.name)),
        Stmt::Interface(i) => declares.push(name_of(src, toks, i.name)),
        Stmt::TypeAlias(t) => declares.push(name_of(src, toks, t.name)),
        Stmt::Remote(r) => declares.push(name_of(src, toks, r.name)),
        Stmt::Message(m) => declares.push(name_of(src, toks, m.name)),
        Stmt::Attribute(a) => declares.push(name_of(src, toks, a.name)),
        Stmt::Macro(m) => declares.push(name_of(src, toks, m.name)),
        Stmt::Class(c) => declares.push(name_of(src, toks, c.name)),
        // An impl on a foreign type, `impl string`, attaches to a name no
        // test declares; its method names are what a test reaches for.
        Stmt::Impl(i) => {
            attaches = Some(name_of(src, toks, i.target));

            for m in &i.methods {
                if let Some(last) = m.path.last() {
                    declares.push(name_of(src, toks, *last));
                }
            }
        }

        _ => {}
    }

    let effect = declares.is_empty()
        && attaches.is_none()
        && !is_test
        && matches!(
            stmt,
            Stmt::NumericFor(_)
                | Stmt::GenericFor(_)
                | Stmt::While(_)
                | Stmt::Repeat(_)
                | Stmt::Do(_)
                | Stmt::Parallel(_)
                | Stmt::If(_)
                | Stmt::Call(..)
                | Stmt::Assign(_)
                | Stmt::Match(_)
        );

    /*
    A use is a name that reads a binding. A field name, in a table or
    after a dot, reads none, and a name that a local or a parameter of
    the statement itself binds reads that one. Each kept a top-level
    function of the same name in the spec, with its imports, and the
    spec failed to load (LANG_BUGS 112).
    */
    let inner = (span.start as usize)..(span.end as usize);
    let bound_inside = |j: usize, name: &str| {
        names.bindings.iter().any(|(n, tok, reach)| {
            n == name
                && inner.contains(tok)
                && reach
                    .as_ref()
                    .is_none_or(|ranges| ranges.iter().any(|&(a, b)| a <= j && j < b))
        })
    };
    // `{ swim: number }` in a type, and `(spec: Spec)`: the name before
    // the annotation's `:` is a key or a parameter. The walk reads no
    // type, so the shape tells it from `obj:method(`.
    let text = |j: usize| toks.get(j).map_or("", |t| t.text(src));
    let annotated = |j: usize| {
        j > 0
            && text(j + 1) == ":"
            && matches!(text(j - 1), "{" | "," | "(")
            && !(toks.get(j + 2).is_some_and(|t| t.kind == TokKind::Ident)
                && matches!(text(j + 3), "(" | "{"))
    };
    let refs = inner
        .clone()
        .filter(|&j| {
            toks[j].kind == TokKind::Ident
                && (!names.members.contains(&j) || names.foreign_methods.contains(text(j)))
        })
        .filter(|&j| !annotated(j) && !bound_inside(j, toks[j].text(src)))
        .map(|j| toks[j].text(src).to_string())
        .collect();

    Decl {
        declares,
        attaches,
        effect,
        refs,
        is_test,
        start,
        end,
    }
}

/// The source cut down to its tests and what they reach, with every
/// other top-level statement blanked so the lines stay. `None` when the
/// file has no `@test`.
///
/// `markup` holds the byte ranges of the markup in a `.alx` file. The
/// parse read them blanked, so the names they use come from their text,
/// with `factory`, the names the lowering calls.
pub fn slice(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    markup: &[(usize, usize)],
    factory: &[&str],
) -> Option<String> {
    let types: HashSet<&str> = chunk
        .block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Struct(d) => Some(d.name),
            Stmt::Enum(d) => Some(d.name),
            Stmt::Class(d) => Some(d.name),
            Stmt::Trait(d) => Some(d.name),
            Stmt::Interface(d) => Some(d.name),
            Stmt::TypeAlias(d) => Some(d.name),

            _ => None,
        })
        .map(|n| n.text(src, toks))
        .collect();
    let foreign_methods = chunk
        .block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Impl(i) if !types.contains(i.target.text(src, toks)) => Some(i),

            _ => None,
        })
        .flat_map(|i| i.methods.iter().filter_map(|m| m.path.last()))
        .map(|n| n.text(src, toks).to_string())
        .collect();
    let names = Names {
        members: crate::naming::member_tokens(src, toks, &chunk.block),
        foreign_methods,
        bindings: crate::naming::scoped_bindings(src, toks, &chunk.block),
    };
    let mut decls: Vec<Decl> = chunk
        .block
        .stmts
        .iter()
        .map(|s| describe(src, toks, s, &names))
        .collect();

    for d in &mut decls {
        for (a, b) in markup.iter().filter(|(a, _)| (d.start..d.end).contains(a)) {
            let words = src[*a..*b]
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'));

            d.refs
                .extend(words.chain(factory.iter().copied()).map(str::to_string));
        }
    }

    if !decls.iter().any(|d| d.is_test) {
        return None;
    }

    let mut selected: Vec<bool> = decls.iter().map(|d| d.is_test).collect();
    let mut needed: HashSet<String> = HashSet::new();

    for (d, s) in decls.iter().zip(&selected) {
        if *s {
            needed.extend(d.refs.iter().cloned());
        }
    }

    // A statement joins when it declares a needed name, or attaches to
    // a name a selected statement declares. Its own references join
    // the needed set, until nothing changes.
    loop {
        let declared_by_selected: HashSet<&String> = decls
            .iter()
            .zip(&selected)
            .filter(|(_, s)| **s)
            .flat_map(|(d, _)| d.declares.iter())
            .collect();
        let mut changed = false;

        for (k, d) in decls.iter().enumerate() {
            if selected[k] {
                continue;
            }

            let declares_needed = d.declares.iter().any(|n| needed.contains(n));
            let attaches_selected = d
                .attaches
                .as_ref()
                .is_some_and(|n| declared_by_selected.contains(n));
            let effect_on_selected =
                d.effect && d.refs.iter().any(|n| declared_by_selected.contains(n));

            if declares_needed || attaches_selected || effect_on_selected {
                selected[k] = true;
                needed.extend(d.refs.iter().cloned());
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    // A kept import keeps the names the kept code reads. A type name
    // needs no require, so `import heavy, { type Shape }` that a test
    // reaches for `Shape` alone loads nothing (LANG_BUGS 119).
    let used: HashSet<&str> = decls
        .iter()
        .zip(&chunk.block.stmts)
        .zip(&selected)
        .filter(|((_, stmt), s)| **s && !matches!(stmt, Stmt::Import(_)))
        .flat_map(|((d, _), _)| d.refs.iter().map(String::as_str))
        .collect();
    let mut blanks: Vec<(usize, usize)> = Vec::new();

    for ((d, stmt), s) in decls.iter().zip(&chunk.block.stmts).zip(&selected) {
        match (stmt, *s) {
            (_, false) => blanks.push((d.start, d.end)),

            (Stmt::Import(i), true) => blanks.extend(unread_import_parts(src, toks, i, &used)),

            _ => {}
        }
    }

    let mut out = src.as_bytes().to_vec();

    for (start, end) in blanks {
        for b in out.iter_mut().take(end).skip(start) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }

    let text = String::from_utf8(out).expect("blanking keeps UTF-8");
    let trimmed: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut joined = trimmed.join("\n");

    if text.ends_with('\n') {
        joined.push('\n');
    }

    Some(joined)
}

/// The byte ranges of an import that bind a name no kept statement
/// reads, each with its comma. A list of names left with types alone
/// is erased to ship, so its module does not load.
fn unread_import_parts(
    src: &str,
    toks: &[Tok],
    i: &Import,
    used: &HashSet<&str>,
) -> Vec<(usize, usize)> {
    let text = |j: u32| toks[j as usize].text(src);
    // The bytes of the tokens `a` up to, not with, `b`.
    let range = |a: u32, b: u32| {
        (
            toks[a as usize].start as usize,
            toks[b as usize - 1].end as usize,
        )
    };
    let bound = |s: &ImportSpec| s.alias.unwrap_or(s.name);
    let read = |s: &ImportSpec| used.contains(text(bound(s).start));
    // The first token of the name for the whole module, and that name.
    let (head, specs) = match &i.kind {
        ImportKind::Default(n) => (Some((n.start, *n)), &[][..]),

        ImportKind::Namespace(n, specs) => (Some((i.span.start + 1, *n)), &specs[..]),

        ImportKind::Both(n, specs) => (Some((n.start, *n)), &specs[..]),

        ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => (None, &specs[..]),
    };
    let head_read = head.is_some_and(|(_, n)| used.contains(text(n.start)));
    let specs_read = specs.iter().any(read);

    if !head_read && !specs_read {
        return vec![range(i.span.start, i.span.end)];
    }

    let mut cuts = Vec::new();

    match head {
        // The name and the comma after it.
        Some((first, n)) if !head_read => cuts.push(range(first, n.end + 1)),

        // The comma and the list, up to `from`.
        Some((_, n)) if !specs_read && !specs.is_empty() => {
            return vec![range(n.end, i.path.start - 1)];
        }

        _ => {}
    }

    for s in specs.iter().filter(|s| !read(s)) {
        let first = s.name.start - u32::from(s.is_type) - u32::from(s.is_attribute);
        let end = bound(s).end + u32::from(text(bound(s).end) == ",");
        cuts.push(range(first, end));
    }

    cuts
}

/// The spec's require target for a path the source requires, relative
/// to the source: the built output when the target is an Alloy file,
/// the file itself otherwise. `None` for a path that is not relative.
fn target_for(
    config: &Config,
    tree: &crate::project::Tree,
    root: &Path,
    source_rel: &Path,
    path: &str,
) -> Option<PathBuf> {
    let base = source_rel.parent().unwrap_or(Path::new(""));
    let joined = if let Some(rest) = path.strip_prefix("./") {
        base.join(rest)
    } else if path.starts_with("../") {
        base.join(path)
    } else {
        let rest = path.strip_prefix('@')?;
        let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let (_, dir) = tree.aliases.iter().find(|(a, _)| a == alias)?;

        dir.join(tail)
    };
    let normal = normalize(&joined);

    // Anything under `in` has a module under the modules folder: a
    // compiled source, a data file, or a plain file copied over.
    if let Ok(under) = normal.strip_prefix(&config.build.input) {
        let input = root.join(&config.build.input);
        let modules = modules_dir(config);

        for ext in ["aly", "alx"] {
            if input.join(under).with_extension(ext).is_file() {
                return crate::build::output_for(&under.with_extension(ext))
                    .map(|o| modules.join(o).with_extension(""));
            }
        }

        // `init.aly` names its directory.
        for ext in ["aly", "alx"] {
            if input.join(under).join(format!("init.{ext}")).is_file() {
                return Some(modules.join(under));
            }
        }

        for ext in ["json", "toml", "luau", "lua"] {
            if input.join(under).with_extension(ext).is_file() {
                return Some(modules.join(under));
            }
        }
    }

    Some(normal)
}

/// The shim's require at the head of a chunk. The shim sets the doubles
/// as globals, so a module that names `Vector3` or `game` at load has
/// them, and takes no local for them. A local for each name cost a
/// module 15 of the 200 that Luau allows (LANG_BUGS 127). The `;` ends
/// the call, so a first line that opens with `(` starts a statement of
/// its own. The line joins the first line, so every later line keeps
/// its number.
fn with_shim(text: &str, shim_require: &str) -> String {
    format!("require({}); {text}", luau_string(shim_require))
}

/// Writes the modules folder: every source compiled with file-path
/// requires, the runtime, every data file as a module, and the plain
/// files. The folder is rebuilt from nothing on each run.
fn write_modules(
    root: &Path,
    config: &Config,
    ingots: &crate::ingot::Ingots,
    extensions: &[crate::extensions::Extension],
) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut failures = Vec::new();
    let input = root.join(&config.build.input);
    let modules = modules_dir(config);
    let dir = root.join(&modules);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    // The runtime reads `Instance`, `game`, `typeof` and `task` as a
    // module does, so it takes the doubles too. Without them `replied`
    // indexed a nil `Instance`, and a timed `destroy` a nil `game`
    // (LANG_BUGS 105).
    let runtime_text = match config.test.shim {
        true => with_shim(crate::RUNTIME, "./shim"),

        false => crate::RUNTIME.to_string(),
    };
    std::fs::write(dir.join("alloy.luau"), runtime_text)?;
    std::fs::write(dir.join("shim.luau"), crate::SHIM)?;
    let exclude = crate::build::globs(&config.build.exclude)?;
    let written = crate::build::written_dirs(root, config);
    let jsx = config.markup(root).ok();
    let runtime = modules.join("alloy");
    let tree = crate::project::Tree::load(root, config);
    let aliases = crate::modules::aliases(root, &tree);
    let sources = crate::build::sources(&input, &written)?;
    // The build's view of other files' structs: an imported struct
    // clones and serializes through its own derives in a test as well.
    let (shapes, wire_scopes) = crate::build::struct_shapes(&sources, &input, &aliases);

    // Each module compiles and writes on its own, so they run on every
    // core, under the run's `Reads` for the modules they import. On one
    // thread the 1070 modules of Strata took minutes.
    let reads = crate::modules::current_reads().unwrap_or_default();
    let compile = |path: &PathBuf| -> std::io::Result<Option<(PathBuf, String)>> {
        let rel = path.strip_prefix(&input).unwrap_or(path).to_path_buf();

        if exclude.is_match(&rel) {
            return Ok(None);
        }

        let Some(rel_out) = crate::build::output_for(&rel) else {
            return Ok(None);
        };
        let module_rel = modules.join(&rel_out);
        let source = std::fs::read_to_string(path)?;
        let source_rel = config.build.input.join(&rel);
        let options = EmitOptions {
            file_name: source_rel.to_string_lossy().into_owned(),
            definitions: rel.to_string_lossy().ends_with(".d.aly"),
            std_require: relative_require(&source_rel, &runtime),
            wait_timeout: config.emit.wait_timeout,
            test_runner: config.test.lest,
            std_globals: config.std.globals.clone(),
            extensions: extensions.to_vec(),
            shapes: shapes.clone(),
            wire_scopes: wire_scopes.clone(),
            late_requires: config.test.shim.then(|| {
                (
                    relative_require(&source_rel, &modules.join("shim")),
                    rel_out
                        .with_extension("")
                        .to_string_lossy()
                        .replace('\\', "/"),
                )
            }),
            // A test reads the ship artifact alone.
            ship_only: true,
            ..EmitOptions::default().imports(&source, path, &aliases)
        };
        let compiled = crate::compile_file(
            &source_rel.to_string_lossy(),
            &source,
            &options,
            jsx.as_ref(),
            Some(ingots),
        );

        match compiled {
            Ok(out) => {
                let mut text =
                    rewrite_requires(config, &tree, root, &source_rel, &module_rel, &out.ship);

                if config.test.shim {
                    text = with_shim(&text, &require_from(&module_rel, &modules.join("shim")));
                }

                let target = dir.join(&rel_out);

                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                std::fs::write(target, text)?;

                Ok(None)
            }

            Err(e) => Ok(Some((rel, e.to_string()))),
        }
    };

    for found in crate::build::par_map(&sources, |path| {
        crate::modules::with_reads(&reads, || compile(path))
    }) {
        failures.extend(found?);
    }

    let mut plain = Vec::new();
    crate::build::walk_plain(&input, &written, &mut plain)?;

    for path in plain {
        let rel = path.strip_prefix(&input).unwrap_or(&path);
        let target = dir.join(rel);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::copy(&path, target)?;
    }

    let mut data = Vec::new();
    crate::build::walk_data(&input, &written, &mut data)?;

    for path in data {
        let rel = path.strip_prefix(&input).unwrap_or(&path);

        if crate::data::module_beside(&path).is_some() {
            continue;
        }

        if let Some(format) = crate::data::Format::of_path(&path)
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            match crate::data::convert(&text, format) {
                Ok(luau) => {
                    let target = dir.join(rel).with_extension("luau");

                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    std::fs::write(target, luau)?;
                }

                Err(e) => failures.push((rel.to_path_buf(), e.to_string())),
            }
        }
    }

    Ok(failures)
}

/// A path with its `.` and `..` components folded.
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

/// The require path from the module at `rel` to `target`. lest reads a
/// relative path in an `init.luau` from the file's own folder, and Luau
/// reads it from the folder above. `@self` names the own folder in both.
fn require_from(rel: &Path, target: &Path) -> String {
    let path = relative_require(rel, target);

    match crate::build::is_init(rel) {
        true => format!("@self/{}", path.strip_prefix("./").unwrap_or(&path)),

        false => path,
    }
}

/// Rewrites every relative or aliased `require` of an emitted text to
/// the path from the spec to the target. The text keeps its line count.
fn rewrite_requires(
    config: &Config,
    tree: &crate::project::Tree,
    root: &Path,
    source_rel: &Path,
    spec_rel: &Path,
    text: &str,
) -> String {
    crate::project::map_requires(text, |path| {
        target_for(config, tree, root, source_rel, path)
            .map(|target| require_from(spec_rel, &target))
    })
}

/// The lest spec of one source: the sliced module, then the `describe`
/// that registers each test. `None` when the source has no `@test`.
pub fn spec(
    config: &Config,
    root: &Path,
    source_rel: &Path,
    source: &str,
    ingots: Option<&crate::ingot::Ingots>,
    extensions: &[crate::extensions::Extension],
) -> Result<Option<(String, Vec<Diagnostic>, usize)>, crate::CompileError> {
    // Markup is no Alloy the parser reads. Blanked, it keeps every
    // offset, so the slice cuts the source itself.
    let alx = source_rel.extension().is_some_and(|e| e == "alx");
    let (markup, blanked) = match alx {
        true => crate::alx::blank_markup(source).unwrap_or_default(),

        false => (Vec::new(), String::new()),
    };
    let parsed = alloy_syntax::parse_lenient(
        if blanked.is_empty() { source } else { &blanked },
        Default::default(),
    )
    .map_err(|e| crate::CompileError {
        offset: e.offset,
        message: e.message,
    })?;
    // A source the lenient parse could not read is no spec: the tree is
    // the recovery's, not the author's, and the slice would register a
    // test the file never wrote. The diagnostics report and the caller
    // writes nothing.
    if !parsed.diagnostics.is_empty() {
        let diagnostics = parsed
            .diagnostics
            .iter()
            .map(|e| Diagnostic {
                start: e.offset as u32,
                end: e.offset as u32,
                message: e.message.clone(),
            })
            .collect();

        return Ok(Some((String::new(), diagnostics, 0)));
    }

    let jsx = alx.then(|| config.markup(root).ok()).flatten();
    // The head name of each expression the lowering calls.
    let factory: Vec<&str> = jsx
        .iter()
        .flat_map(|c| {
            [
                Some(c.create.as_str()),
                c.fragment.as_deref(),
                c.compute.as_deref(),
                c.merge.as_deref(),
                c.children.as_deref(),
                c.event.as_ref().map(|e| e.expression()),
            ]
        })
        .flatten()
        .filter_map(|e| {
            e.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
        })
        .collect();
    let Some(sliced) = slice(source, &parsed.lexed.toks, &parsed.chunk, &markup, &factory) else {
        return Ok(None);
    };
    let spec_rel = config.test.out.join(
        spec_for(
            source_rel
                .strip_prefix(&config.build.input)
                .unwrap_or(source_rel),
        )
        .unwrap_or_default(),
    );
    // The require is written from the source's place, as every other
    // require in the text, and the rewrite below moves them all.
    let runtime = modules_dir(config).join("alloy");
    let tree = crate::project::Tree::load(root, config);
    let aliases = crate::modules::aliases(root, &tree);
    // ponytail: every spec reads the project's structs again; cache them
    // per run if a project with many specs makes `alloy test` slow.
    let input = root.join(&config.build.input);
    let (shapes, wire_scopes) =
        crate::build::sources(&input, &crate::build::written_dirs(root, config))
            .map(|s| crate::build::struct_shapes(&s, &input, &aliases))
            .unwrap_or_default();
    let options = EmitOptions {
        file_name: source_rel.to_string_lossy().into_owned(),
        std_require: relative_require(source_rel, &runtime),
        shapes,
        wire_scopes,
        tests: true,
        wait_timeout: config.emit.wait_timeout,
        test_runner: config.test.lest,
        std_globals: config.std.globals.clone(),
        extensions: extensions.to_vec(),
        ..EmitOptions::default().imports(source, &root.join(source_rel), &aliases)
    };
    let out = crate::compile_file(
        &source_rel.to_string_lossy(),
        &sliced,
        &options,
        jsx.as_ref(),
        ingots,
    )?;
    let mut text = rewrite_requires(config, &tree, root, source_rel, &spec_rel, &out.ship);
    let shim = relative_require(&spec_rel, &modules_dir(config).join("shim"));

    if config.test.shim {
        text = with_shim(&text, &shim);
    }

    let name = source_rel
        .strip_prefix(&config.build.input)
        .unwrap_or(source_rel)
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");

    if !text.ends_with('\n') {
        text.push('\n');
    }

    // `LEST_ALIAS` is the one place that names the runner: the spec
    // requires it here and hands its `expect` to the runtime, and
    // `$expect` calls the runtime. A second runner is that name and an
    // `expect(value)` of the same shape.
    text.push_str(&format!(
        "\nlocal __lest = require(\"@{}\")\n",
        LEST_ALIAS.0
    ));
    // `$expect(v)` reaches the matchers through the runtime.
    text.push_str("__alloy.set_expect(__lest.expect)\n");

    // The modules that scoped imports name load now, while lest still
    // resolves a require. The shim calls the spec's own `require`, with
    // the path from the spec to the modules. An async test hands its
    // Future to the shim, so the spec binds the shim only when it has
    // one.
    if config.test.shim {
        let shim = luau_string(&shim);
        let preload = format!(
            "preload({}, function(path) return require(path) end)",
            luau_string(&format!(
                "{}/",
                relative_require(&spec_rel, &modules_dir(config))
            ))
        );

        match out.tests.iter().any(|(_, is_async)| *is_async) {
            true => text.push_str(&format!(
                "local __shim = require({shim})\n__shim.{preload}\n"
            )),

            false => text.push_str(&format!("require({shim}).{preload}\n")),
        }
    }

    text.push_str(&format!(
        "__lest.describe({}, function()\n",
        luau_string(&name)
    ));

    for (test, is_async) in &out.tests {
        if *is_async {
            // lest calls the test on a thread that cannot yield. The shim
            // steps the threads a `task.wait` parked until the Future
            // settles, so the `await` here returns at once.
            let future = if config.test.shim {
                format!("__shim.settle({test}())")
            } else {
                format!("{test}()")
            };
            text.push_str(&format!(
                "    __lest.it({}, function()\n        __alloy.await({future})\n    end)\n",
                luau_string(test)
            ));
        } else {
            text.push_str(&format!("    __lest.it({}, {test})\n", luau_string(test)));
        }
    }

    text.push_str("end)\n");

    Ok(Some((text, out.diagnostics, out.tests.len())))
}

fn luau_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The `lest.toml` `alloy test` writes when the root has none.
pub fn lest_toml(config: &Config) -> String {
    let out = config.test.out.to_string_lossy().replace('\\', "/");

    format!(
        "# Written by `alloy test`. `lest` runs the suite; `alloy test` rewrites the specs.\n\n[suites.{}]\ninclude = [\"{out}/**/*.spec.luau\"]\n\n[settings]\nbackend = \"native\"\ntimeout_ms = 5000\n",
        config.test.suite
    )
}

/// Writes the specs of a project under `[test] out`. With `write`
/// false, nothing changes and `stale` lists the specs that would.
pub fn run(root: &Path, config: &Config, write: bool) -> std::io::Result<Report> {
    /*
    Each module and each spec compiles once more here, and every compile
    reads all its imports again. With no `Reads`, the run read 347 MB on
    Strata, where the build reads 12 MB. On the NTFS disk under FUSE, one
    daemon thread serves every read, so four runs at once took more than
    10 minutes and looked like a deadlock with the ingots.
    */
    let reads = std::sync::Arc::new(crate::modules::Reads::default());

    crate::modules::with_reads(&reads, || write_specs(root, config, write))
}

fn write_specs(root: &Path, config: &Config, write: bool) -> std::io::Result<Report> {
    let mut report = Report::default();
    let input = root.join(&config.build.input);
    let out_dir = root.join(&config.test.out);
    let mut expected: HashSet<PathBuf> = HashSet::new();
    let exclude = crate::build::globs(&config.build.exclude)?;
    let written = crate::build::written_dirs(root, config);
    let ingots = crate::ingot::Ingots::load(root, config);

    for p in &ingots.problems {
        report.failures.push((
            PathBuf::from(
                Config::file_of(root)
                    .file_name()
                    .unwrap_or(crate::config::FILE_NAME.as_ref()),
            ),
            p.to_string(),
        ));
    }

    // Extensions are project wide, as in the build: a call by an
    // extension name routes through the dispatcher in every spec.
    let mut extensions = Vec::new();

    for path in crate::build::sources(&input, &written)? {
        if path.extension().is_some_and(|e| e == "aly")
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            extensions.extend(crate::extensions::collect(&source));
        }
    }

    if write {
        for (rel, message) in write_modules(root, config, &ingots, &extensions)? {
            report.failures.push((rel, message));
        }
    }

    let mut spec_owners: HashMap<PathBuf, PathBuf> = HashMap::new();

    for path in crate::build::sources(&input, &written)? {
        let rel = path.strip_prefix(&input).unwrap_or(&path).to_path_buf();

        if exclude.is_match(&rel) {
            continue;
        }

        let Some(spec_rel) = spec_for(&rel) else {
            continue;
        };
        let source = std::fs::read_to_string(&path)?;

        // `b.aly` and `b.spec.aly` both name `b.spec.luau`; a second
        // write would drop the first file's tests.
        if source.lines().any(writes_test_attr)
            && let Some(first) = spec_owners.insert(spec_rel.clone(), rel.clone())
        {
            report.failures.push((
                rel.clone(),
                format!(
                    "{} and {} both write the spec {}; rename one",
                    config.build.input.join(&first).display(),
                    config.build.input.join(&rel).display(),
                    config.test.out.join(&spec_rel).display(),
                ),
            ));

            continue;
        }
        let source_rel = config.build.input.join(&rel);

        let built = match spec(
            config,
            root,
            &source_rel,
            &source,
            Some(&ingots),
            &extensions,
        ) {
            Ok(Some(b)) => b,

            Ok(None) => continue,

            Err(e) => {
                report.failures.push((rel, e.to_string()));

                continue;
            }
        };
        let (text, diagnostics, count) = built;
        report.tests += count;

        for d in diagnostics {
            report.diagnostics.push((rel.clone(), d));
        }

        let target = out_dir.join(&spec_rel);
        expected.insert(target.clone());

        // `spec` gives no text for a source that does not parse. The
        // diagnostics above count it; the spec it wrote before stays, so
        // a syntax error deletes nothing.
        if text.is_empty() {
            continue;
        }
        let shown = config.test.out.join(&spec_rel);
        let current = std::fs::read_to_string(&target).ok();

        if !write {
            if current.as_deref() != Some(text.as_str()) {
                report.stale.push(shown);
            }

            continue;
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if current.as_deref() != Some(text.as_str()) {
            std::fs::write(&target, &text)?;
        }

        report.written.push(shown);
    }

    // A spec whose source lost its tests goes. The removal is a change,
    // so `--check` counts the orphan as stale and fails on it.
    if out_dir.is_dir() {
        let mut all = Vec::new();
        walk(&out_dir, &mut all)?;

        for file in all {
            if !file.to_string_lossy().ends_with(".spec.luau") || expected.contains(&file) {
                continue;
            }

            let shown = file.strip_prefix(root).unwrap_or(&file).to_path_buf();

            if !write {
                report.stale.push(shown);

                continue;
            }

            std::fs::remove_file(&file)?;
            report.removed.push(shown);
        }
    }

    if !write {
        return Ok(report);
    }

    if config.test.lest && !report.written.is_empty() {
        let toml = root.join("lest.toml");

        if !toml.is_file() {
            std::fs::write(&toml, lest_toml(config))?;
            report
                .notes
                .push(format!("wrote lest.toml: suite `{}`", config.test.suite));
        }

        lest_alias(root, &mut report)?;
    }

    Ok(report)
}

/// The alias lest resolves to the framework it writes under
/// `.lest/core`.
const LEST_ALIAS: (&str, &str) = ("lest", ".lest/core");

/// Adds the `@lest` alias to the Luau configuration of the root, the
/// file lest and the build read. A file the editor cannot change gets
/// a note that says why.
fn lest_alias(root: &Path, report: &mut Report) -> std::io::Result<()> {
    // `.config.luau` wins over `.luaurc`, as in Luau.
    let Some(path) = [".config.luau", ".luaurc"]
        .iter()
        .map(|name| root.join(name))
        .find(|p| p.is_file())
    else {
        let c = crate::luau_config::LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: vec![(LEST_ALIAS.0.to_string(), LEST_ALIAS.1.to_string())],
        };
        std::fs::write(root.join(".luaurc"), crate::luau_config::render_luaurc(&c))?;
        report
            .notes
            .push("wrote .luaurc: strict mode and the @lest alias".to_string());

        return Ok(());
    };
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let text = std::fs::read_to_string(&path)?;
    let edited = if name == ".config.luau" {
        crate::luau_config::add_alias_config_luau(&text, LEST_ALIAS)
    } else {
        crate::luau_config::add_alias_luaurc(&text, LEST_ALIAS)
    };

    match edited {
        Ok(Some(text)) => {
            std::fs::write(&path, text)?;
            report
                .ok
                .push(format!("added {} to the aliases of {name}", LEST_ALIAS.0));
        }

        // The file carries the alias already.
        Ok(None) => {}

        Err(e) => report.notes.push(format!(
            "add `{} = \"{}\"` to the aliases of {name}: {e}",
            LEST_ALIAS.0, LEST_ALIAS.1
        )),
    }

    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sliced(src: &str) -> Option<String> {
        let parsed = alloy_syntax::parse_lenient(src, Default::default())
            .ok()
            .unwrap();

        slice(src, &parsed.lexed.toks, &parsed.chunk, &[], &[])
    }

    /*
    A test keeps what it reaches by name, not by word. A field name, in a
    table or after a dot, a word in a comment, and a local or a parameter
    of the test named like a function of the file are no use of that
    function. Each kept one here, with the imports it read, and the spec
    failed to load (LANG_BUGS 112). A real call still keeps it.
    */
    #[test]
    fn a_test_keeps_what_it_reaches_by_name() {
        let src = concat!(
            "import { scheduler } from './scheduler'\n",
            "\n",
            "type Spec = { swim: number, rank: number }\n",
            "\n",
            "local function rise(spec: Spec): number\n",
            "    return spec.swim\n",
            "end\n",
            "\n",
            "local function swim()\n",
            "    scheduler:getDeltaTime()\n",
            "end\n",
            "\n",
            "local function rank(): number\n",
            "    return 1\n",
            "end\n",
            "\n",
            "local function strike(): number\n",
            "    return 2\n",
            "end\n",
            "\n",
            "local function used(): number\n",
            "    return 3\n",
            "end\n",
            "\n",
            "@test\n",
            "function rises()\n",
            "    -- The rank of the swim does not matter here.\n",
            "    const strike = { rank = 1 }\n",
            "    $assert_eq(rise({ swim = 2, rank = strike.rank }), 2)\n",
            "    $assert_eq(used(), 3)\n",
            "end\n",
        );
        let out = sliced(src).unwrap();

        for kept in ["local function rise", "local function used", "type Spec"] {
            assert!(out.contains(kept), "{kept}\n{out}");
        }

        for gone in [
            "local function swim",
            "local function rank",
            "local function strike",
            "import { scheduler }",
        ] {
            assert!(!out.contains(gone), "{gone}\n{out}");
        }

        // A method of an impl on a foreign type is reached by its name
        // after `:`, and the impl stays.
        let foreign = "impl string as\n    function shout(self): string\n        return self:upper()\n    end\nend\n\n@test\nfunction shouts()\n    $assert_eq((\"a\"):shout(), \"A\")\nend\n";
        let out = sliced(foreign).unwrap();
        assert!(out.contains("impl string"), "{out}");
    }

    #[test]
    fn a_file_without_tests_has_no_spec() {
        assert_eq!(sliced("local x = 1\nprint(x)\n"), None);
    }

    #[test]
    fn the_slice_keeps_what_the_tests_reach_and_the_lines() {
        let src = "import { helper } from \"./util\"\nimport { other } from \"./other\"\nlocal Players = game:GetService(\"Players\")\n\nlocal function used(): number\n    return helper(1)\nend\n\nlocal function unused(): number\n    return other(2)\nend\n\nPlayers.PlayerAdded:Connect(function() end)\n\n@test\nfunction it_works()\n    $assert_eq(used(), 1)\nend\n\nreturn { used = used }\n";
        let out = sliced(src).unwrap();
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(out.contains("import { helper } from \"./util\""));
        assert!(!out.contains("other"));
        assert!(!out.contains("Players"));
        assert!(out.contains("local function used()"));
        assert!(!out.contains("unused"));
        assert!(!out.contains("return { used"));
        assert!(out.contains("function it_works()"));
    }

    #[test]
    fn an_impl_follows_its_struct() {
        let src = "struct V as\n    x: number\nend\n\nimpl V as\n    function len(self): number\n        return self.x\n    end\nend\n\nstruct W as\n    y: number\nend\n\n@test\nfunction v_len()\n    $assert_eq((new V { x = 2 }):len(), 2)\nend\n";
        let out = sliced(src).unwrap();
        assert!(out.contains("impl V"));
        assert!(!out.contains("struct W"));
    }

    /// A spec whose source lost its last `@test` is stale under
    /// `--check` and goes under `alloy test`.
    #[test]
    fn an_orphan_spec_is_stale_under_check() {
        let dir = std::env::temp_dir().join(format!("alloy-orphan-spec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::create_dir_all(dir.join("tests")).expect("the folder");
        std::fs::write(dir.join("src/main.aly"), "local x = 1\n").expect("the file");
        std::fs::write(dir.join("tests/main.spec.luau"), "return {}\n").expect("the file");

        let config = Config::default();
        let report = run(&dir, &config, false).expect("the check");

        assert_eq!(report.stale, [PathBuf::from("tests/main.spec.luau")]);
        assert!(!report.is_clean());

        let report = run(&dir, &config, true).expect("the write");

        assert_eq!(report.removed, [PathBuf::from("tests/main.spec.luau")]);
        assert!(!dir.join("tests/main.spec.luau").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes `files` into a new project, builds its specs, and runs lest
    /// there. The output of a run that passed, or `None` where lest is
    /// not installed and the test skips.
    fn lest_run(name: &str, files: &[(&str, &str)]) -> Option<String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let Some(lest) = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .chain([home.join(".ember/bin")])
            .map(|d| d.join("lest"))
            .find(|p| p.is_file())
        else {
            eprintln!("skipped: lest is not installed");

            return None;
        };

        let dir = std::env::temp_dir().join(format!("alloy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        for (path, text) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().expect("the folder")).expect("the folder");
            std::fs::write(path, text).expect("the file");
        }

        let config = Config::default();
        let report = run(&dir, &config, true).expect("the write");
        assert!(
            report.is_clean(),
            "{:?} {:?}",
            report.diagnostics,
            report.failures
        );

        let out = std::process::Command::new(lest)
            .current_dir(&dir)
            .arg(&config.test.suite)
            .output()
            .expect("lest runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(out.status.success(), "{text}");

        Some(text)
    }

    /*
    The runtime copy under `tests/.modules` took none of the shim's
    doubles, so `Instance`, `game` and `task` were nil there. A message's
    `replied` failed with `attempt to index nil with 'new'`, and a timed
    `destroy` of an Instance with one on `GetService` (LANG_BUGS 105).
    The copy now takes the doubles, as every module does, and lest runs
    both. The test skips where lest is not installed.
    */
    #[test]
    fn the_test_runtime_takes_the_shim() {
        let Some(text) = lest_run(
            "test-shim",
            &[(
                "src/jobs.aly",
                "--- A job, and its answer.\nexport message Job(job: number) reply(job: number)\n\n@test\nfunction a_reply_binds() -> ()\n    local got = 0\n    Job.replied(function(job: number) -> () got = job end)\n    $assert_eq(got, 0)\nend\n\nlocal function part(): any\n    return new Instance('Part')\nend\n\n@test\nfunction a_timed_destroy_waits() -> ()\n    const p = part()\n    p.Parent = workspace\n    destroy p after 1\n    $assert_eq(p.Parent, workspace)\nend\n",
            )],
        ) else {
            return;
        };

        assert!(text.contains("2 passed"), "{text}");
    }

    /*
    The shim bound the doubles as locals of each Alloy module, so a plain
    package that reads `game` as a global found nil and failed to load
    (LANG_BUGS 125). The locals also took 15 of the 200 that Luau allows
    a function, so a module with 190 locals loaded in the game and not in
    a test (LANG_BUGS 127). The shim now sets the doubles as globals, and
    a module takes none of its locals for them.
    */
    #[test]
    fn a_package_reads_the_doubles_as_globals() {
        let Some(text) = lest_run(
            "shim-globals",
            &[
                (
                    "vendor/pkg.luau",
                    "local Players = game:GetService(\"Players\")\nreturn { name = function() return \"players: \" .. tostring(Players ~= nil) .. \" \" .. typeof(Vector3.new()) end }\n",
                ),
                (
                    "src/main.aly",
                    "import pkg from '../vendor/pkg'\n\n@test\nfunction a_package_that_reads_game_loads() -> ()\n  $assert_eq(pkg.name(), 'players: true Vector3')\nend\n",
                ),
            ],
        ) else {
            return;
        };

        assert!(text.contains("1 passed"), "{text}");
    }

    #[test]
    fn a_module_near_the_local_limit_loads_in_a_test() {
        // Luau folds a local that holds a constant, and such a local
        // takes no register, so each one holds a call.
        let locals: String = (0..190)
            .map(|i| format!("local c{i} = tonumber('{i}')\n"))
            .collect();
        let names: Vec<String> = (0..190).map(|i| format!("c{i}")).collect();
        let many = format!(
            "{locals}\n--- Every constant.\nexport function all() -> {{ number }}\n  return {{ {} }}\nend\n\n@test\nfunction it_loads() -> ()\n  $assert_eq(#all(), 190)\nend\n",
            names.join(", ")
        );
        let Some(text) = lest_run(
            "local-limit",
            &[
                ("src/many.aly", many.as_str()),
                (
                    "src/use.aly",
                    "import { all } from './many'\n\n@test\nfunction the_module_loads() -> ()\n  $assert_eq(all()[190], 189)\nend\n",
                ),
            ],
        ) else {
            return;
        };

        assert!(text.contains("2 passed"), "{text}");
    }

    /*
    A scoped import requires its module at the call. lest's VM resolves a
    `require` only while a spec loads, so a test in another module that
    reached one failed (LANG_BUGS 120). The spec now loads each module a
    scoped import names before its load ends. Here the import breaks a
    cycle, the test lives in a folder of its own, and an `init` module
    holds a second import.
    */
    #[test]
    fn a_scoped_import_loads_for_a_test_in_another_module() {
        let Some(text) = lest_run(
            "scoped-import",
            &[
                (
                    "src/b.aly",
                    "import { four } from './parts/a'\n\nexport function two() -> number\n  return 2\nend\n\nexport function eight() -> number\n  return four() * 2\nend\n",
                ),
                (
                    "src/parts/a.aly",
                    "export function four() -> number\n  import { two } from '../b'\n\n  return two() * 2\nend\n",
                ),
                (
                    "src/lib/init.aly",
                    "export function three() -> number\n  import { two } from '../b'\n\n  return two() + 1\nend\n",
                ),
                (
                    "src/deep/c.aly",
                    "import { four } from '../parts/a'\nimport { three } from '../lib'\n\n@test\nfunction four_is_four() -> ()\n  $assert_eq(four(), 4)\nend\n\n@test\nasync function three_is_three() -> ()\n  $assert_eq(three(), 3)\nend\n",
                ),
            ],
        ) else {
            return;
        };

        assert!(text.contains("2 passed"), "{text}");
    }

    /// A run compiles each module and each spec, and each compile reads
    /// its imports many times. One `Reads` for the run keeps that to one
    /// disk read a file. With none, the reads of `alloy test` on a slow
    /// disk took more than 10 minutes and looked like a hang.
    #[test]
    fn a_run_reads_each_module_once() {
        let dir = std::env::temp_dir().join(format!("alloy-reads-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::write(dir.join("src/shared.aly"), "export const K = 1\n").expect("the file");

        for i in 0..6 {
            std::fs::write(
                dir.join(format!("src/m{i}.aly")),
                format!(
                    "import {{ K }} from \"./shared\"\n\n@test\nfunction t{i}()\n    $assert_eq(K, 1)\nend\n"
                ),
            )
            .expect("the file");
        }

        crate::modules::DISK_READS.with(|n| n.set(0));
        let report = run(&dir, &Config::default(), true).expect("the write");
        let reads = crate::modules::DISK_READS.with(std::cell::Cell::get);

        assert_eq!(report.tests, 6, "{:?}", report.failures);
        assert!(reads <= 7, "{reads} reads of 7 files");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A barrel `lib/init.aly` wrote `require("./lib/m")` and
    /// `require("../shim")` in its test copy, from the folder above, as
    /// Luau reads an `init.luau`. lest reads them from the file's own
    /// folder, so no spec that reached the barrel could load. `@self`
    /// names that folder for both.
    #[test]
    fn an_init_module_requires_from_its_own_folder() {
        let dir = std::env::temp_dir().join(format!("alloy-init-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/lib")).expect("the folder");
        std::fs::write(dir.join("src/lib/init.aly"), "export { X } from \"./m\"\n")
            .expect("the file");
        std::fs::write(dir.join("src/lib/m.aly"), "export const X = 1\n").expect("the file");
        std::fs::write(
            dir.join("src/use.aly"),
            "import { X } from \"./lib\"\n\n@test\nfunction one()\n    $assert_eq(X, 1)\nend\n",
        )
        .expect("the file");

        let mut config = Config::default();
        config.test.shim = true;
        run(&dir, &config, true).expect("the write");

        let init = dir.join("tests/.modules/lib/init.luau");
        let text = std::fs::read_to_string(&init).expect("the module");
        let folder = init.parent().expect("the folder");

        for (spec, file) in [("@self/m", "m.luau"), ("@self/../shim", "../shim.luau")] {
            assert!(text.contains(&format!("require(\"{spec}\")")), "{text}");
            assert!(folder.join(file).is_file(), "{spec} names no module");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `alloy test` adds the `@lest` alias to the Luau configuration
    /// the project has. A note that names the file is the last resort,
    /// not the first.
    #[test]
    fn the_lest_alias_lands_in_the_luau_configuration() {
        let dir = std::env::temp_dir().join(format!("alloy-lest-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder");

        // `.config.luau`, the file `alloy init` writes.
        let path = dir.join(".config.luau");
        std::fs::write(&path, crate::config::CONFIG_LUAU_TEMPLATE).expect("the file");

        let mut report = Report::default();
        lest_alias(&dir, &mut report).expect("the edit");
        let text = std::fs::read_to_string(&path).expect("the file");
        let back = crate::luau_config::parse_config_luau(&text).expect("the chunk");

        assert_eq!(
            back.aliases,
            vec![
                ("alloy".to_string(), "./build/alloy".to_string()),
                ("lest".to_string(), ".lest/core".to_string()),
            ],
            "{text}"
        );
        assert_eq!(report.ok, ["added lest to the aliases of .config.luau"]);
        assert!(report.notes.is_empty(), "{:?}", report.notes);

        // A second run changes nothing.
        let mut again = Report::default();
        lest_alias(&dir, &mut again).expect("the edit");
        assert_eq!(std::fs::read_to_string(&path).expect("the file"), text);
        assert!(again.ok.is_empty() && again.notes.is_empty());

        // A `.luaurc` keeps its comments and its own aliases.
        std::fs::remove_file(&path).expect("the file");
        let rc = dir.join(".luaurc");
        std::fs::write(
            &rc,
            "{\n  // ours\n  \"aliases\": { \"pkg\": \"Packages\" }\n}\n",
        )
        .expect("the file");

        let mut report = Report::default();
        lest_alias(&dir, &mut report).expect("the edit");
        let text = std::fs::read_to_string(&rc).expect("the file");

        assert!(text.contains("// ours"), "{text}");
        assert!(text.contains("\"pkg\""), "{text}");
        assert!(text.contains("\".lest/core\""), "{text}");
        assert_eq!(report.ok, ["added lest to the aliases of .luaurc"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_point_from_the_spec_at_the_build() {
        assert_eq!(
            relative_require(Path::new("tests/a/b.spec.luau"), Path::new("build/a/c")),
            "../../build/a/c"
        );
        assert_eq!(
            relative_require(Path::new("tests/b.spec.luau"), Path::new("build/alloy")),
            "../build/alloy"
        );
        assert_eq!(
            spec_for(Path::new("a/b.aly")),
            Some(PathBuf::from("a/b.spec.luau"))
        );
        assert_eq!(spec_for(Path::new("g.d.aly")), None);
        assert_eq!(
            spec_for(Path::new("a/loot.spec.aly")),
            Some(PathBuf::from("a/loot.spec.luau"))
        );
        assert_eq!(
            spec_for(Path::new("loot.test.aly")),
            Some(PathBuf::from("loot.spec.luau"))
        );
        assert_eq!(
            spec_for(Path::new("ui/card.alx")),
            Some(PathBuf::from("ui/card.spec.luau"))
        );
    }

    #[test]
    fn the_spec_registers_each_test_and_awaits_the_async_ones() {
        let src = "local function f(): number\n    return 1\nend\n\n@test\nfunction plain()\n    $assert_eq(f(), 1)\nend\n\n@test\nasync function later()\n    local v = await Future.delay(0)\n    $assert(v == nil)\nend\n";
        let config = Config::default();
        let (text, diagnostics, count) = spec(
            &config,
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 2);
        assert!(
            text.contains("local __alloy = require(\"./.modules/alloy\")"),
            "{text}"
        );
        assert!(text.contains("__lest.describe(\"m\", function()"), "{text}");
        assert!(text.contains("__lest.it(\"plain\", plain)"), "{text}");
        // lest calls a test on a thread that cannot yield, so the shim
        // steps the parked threads until the Future settles.
        assert!(
            text.contains("__alloy.await(__shim.settle(later()))"),
            "{text}"
        );
        assert!(!text.contains("__alloy.test("), "{text}");

        // Without the shim nothing parks, and the Future goes to `await`.
        let mut plain = Config::default();
        plain.test.shim = false;
        let (text, _, _) = spec(
            &plain,
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert!(text.contains("__alloy.await(later())"), "{text}");
    }

    /// A `.alx` file writes a spec too. The slice follows the names the
    /// markup uses, and the markup lowers with the project's config.
    #[test]
    fn a_markup_file_writes_a_spec() {
        let src = "local function create(name: any): any\n    return function(props: any): any return props end\nend\n\nlocal function Card(props: { title: string }): any\n    return <TextLabel Text={props.title} />\nend\n\nlocal unused = 1\n\n@test\nfunction builds_a_card()\n    local card = <Card title=\"x\" />\n    $assert(card ~= nil)\nend\n";
        let config = Config::parse(
            "[alx.factory]\nbackend = \"table\"\ncreate = \"create\"\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        let (text, diagnostics, count) = spec(
            &config,
            Path::new("/none"),
            Path::new("src/m.alx"),
            src,
            None,
            &[],
        )
        .unwrap()
        .expect("a spec");

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 1);
        assert!(text.contains("create(\"TextLabel\")"), "{text}");
        assert!(text.contains("local function Card"), "{text}");
        assert!(!text.contains("unused"), "{text}");
    }

    /// The text of a `<style>` element is CSS, as the build reads it.
    /// The scan for markup read `--soon` as a comment, so the `}` and the
    /// `</style>` after it fell into the comment and no spec was written.
    #[test]
    fn a_style_block_is_no_code_to_the_spec() {
        let src = "local function Panel(): any\n    return (\n        <div>\n            <style>\n                :root { --soon: red; }\n                .soon { color: var(--soon); }\n            </style>\n        </div>\n    )\nend\n\n@test\nfunction adds()\n    $assert(1 + 1 == 2)\nend\n";
        let config = Config::parse(
            "[alx.factory]\nbackend = \"table\"\ncreate = \"create\"\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        let (text, diagnostics, count) = spec(
            &config,
            Path::new("/none"),
            Path::new("src/m.alx"),
            src,
            None,
            &[],
        )
        .unwrap()
        .expect("a spec");

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 1);
        assert!(text.contains("__lest.it(\"adds\", adds)"), "{text}");
    }

    /// A namespace member renders under its own name and the table
    /// carries it, so the spec calls the test by its path.
    #[test]
    fn a_test_inside_a_namespace_registers_by_its_path() {
        let src = "namespace Suite as\n    @test\n    function ns_case()\n        $assert(1 == 1)\n    end\nend\n";
        let (text, _, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(count, 1);
        assert!(
            text.contains("__lest.it(\"Suite.ns_case\", Suite.ns_case)"),
            "{text}"
        );
        assert!(text.contains("Suite.ns_case = Suite_ns_case"), "{text}");
    }

    /// `@test` on the group makes every public function of it a test.
    /// A private member and a member that binds no function stay out,
    /// a member with its own `@test` registers once, and the flag
    /// reaches a public nested namespace.
    #[test]
    fn a_test_namespace_registers_each_public_function() {
        let src = "@test
namespace Suite as
    const LIMIT = 4

    private function helper(n: number): number
        return n * 2
    end

    function plain()
        $assert_eq(helper(2), LIMIT)
    end

    @test
    function marked()
        $assert(true)
    end

    namespace Inner as
        function deep()
            $assert(true)
        end
    end

    private namespace Hidden as
        function skipped()
            $assert(true)
        end
    end
end
";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 3);
        assert!(
            text.contains("__lest.it(\"Suite.plain\", Suite.plain)"),
            "{text}"
        );
        assert!(
            text.contains("__lest.it(\"Suite.marked\", Suite.marked)"),
            "{text}"
        );
        assert!(
            text.contains("__lest.it(\"Suite.Inner.deep\", Suite.Inner.deep)"),
            "{text}"
        );
        // A private member, a member that binds no function, and a
        // private nested namespace register nothing.
        assert!(!text.contains("__lest.it(\"Suite.helper"), "{text}");
        assert!(!text.contains("__lest.it(\"Suite.LIMIT"), "{text}");
        assert!(!text.contains("__lest.it(\"Suite.Hidden"), "{text}");
    }

    /// `$assert` lowers to the Luau `assert`, so the body needs nothing
    /// of the runtime. The spec still calls `__alloy.set_testing`, on
    /// the first line, so `@cfg(test)` holds while the module loads.
    /// An import that only a test reads left its `require` in the
    /// build. It leaves with the test now, and the spec keeps it. An
    /// import in a test required its module when the test ran, and
    /// lest's native backend resolves a `require` only while the spec
    /// loads. The spec now requires it on the first line.
    #[test]
    fn a_test_only_import_leaves_the_build_and_loads_with_the_spec() {
        let top = "import { use_base } from './testing'\n\nexport function content(): number\n    return 1\nend\n\n@test\nfunction loads()\n    use_base()\nend\n";
        let ship = crate::compile(top).unwrap().ship;
        assert!(!ship.contains("require('./testing')"), "{ship}");

        // A read outside the test keeps the import.
        let kept = top.replace("return 1", "use_base()\n    return 1");
        let ship = crate::compile(&kept).unwrap().ship;
        assert!(ship.contains("require('./testing')"), "{ship}");

        let inner = "export function inner(): number\n    return 2\nend\n\n@test\nfunction loads()\n    import { use_base } from './testing'\n    use_base()\nend\n";
        let (text, _, _) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            inner,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        let head = text.lines().find(|l| l.contains("set_testing")).unwrap();
        assert!(head.contains("local _m1 = require("), "{text}");
        assert_eq!(text.matches("require('").count(), 1, "{text}");
        assert!(
            text.contains("    local use_base = _m1.use_base\n"),
            "{text}"
        );
    }

    #[test]
    fn a_spec_requires_the_runtime_it_calls() {
        let src = "@test
local function only_assert()
    $assert(1 == 1)
end
";
        let (text, _, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(count, 1);
        assert!(
            text.contains(
                "local __alloy = require(\"./.modules/alloy\") __alloy.set_testing(true)"
            ),
            "{text}"
        );
        assert_eq!(text.matches("set_testing").count(), 1, "{text}");
    }

    /// `export default function tick` declares `tick`, as the plain
    /// declaration does. The slice read no name through the export, so
    /// the spec held blank lines where the function was, and the test
    /// called nil (LANG_BUGS 131).
    #[test]
    fn a_default_function_stays_in_its_spec() {
        let src = "--- One tick.\nexport default function tick(n: number) -> number\n  return n + 1\nend\n\n@test\nfunction ticks() -> ()\n  $assert_eq(tick(1), 2)\nend\n";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/tick.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 1);
        assert!(text.contains("local function tick("), "{text}");
    }

    /// A test that reached a type of a mixed import kept the whole
    /// import, so the spec required the module for a name the ship
    /// erases, and the module failed the load (LANG_BUGS 119). The slice
    /// now keeps the names the kept code reads, and a list of types
    /// requires nothing.
    #[test]
    fn a_type_of_a_mixed_import_requires_nothing() {
        let src = "import heavy, { type Shape } from './heavy'\nimport { type Size, scale, unused } from './sizes'\nimport * as Whole, { type Part } from './whole'\n\nexport function area(shape: Shape, size: Size, part: Part) -> number\n  return scale(shape.size * size)\nend\n\nexport function use_value() -> number\n  return heavy + unused + Whole.n\nend\n\n@test\nfunction area_reads_the_size() -> ()\n  $assert_eq(area({ size = 3 }, 3, {}), 9)\nend\n";
        let out = sliced(src).unwrap();

        assert_eq!(out.lines().count(), src.lines().count());
        assert!(
            out.contains("import        { type Shape } from './heavy'"),
            "{out}"
        );
        assert!(
            out.contains("import { type Size, scale,        } from './sizes'"),
            "{out}"
        );
        assert!(
            out.contains("import             { type Part } from './whole'"),
            "{out}"
        );

        let (text, diagnostics, _) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/mixed.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(!text.contains("heavy") && !text.contains("whole"), "{text}");
        assert!(text.contains("/sizes"), "{text}");
    }

    /// A source that does not parse gives the recovery's tree, not the
    /// author's, and the slice read a `@test` out of it. The spec built
    /// and the run passed. The parse diagnostics now come back with no
    /// text, so the caller reports them and writes nothing.
    #[test]
    fn a_source_that_does_not_parse_writes_no_spec() {
        let src =
            "namespace A {\n    @test\n    function name()\n        $assert(1 == 1)\n    end\n}\n";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/broken.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();

        assert!(text.is_empty(), "{text}");
        assert_eq!(count, 0);
        assert!(!diagnostics.is_empty());
        assert!(
            diagnostics.iter().any(|d| d
                .message
                .contains("a namespace body goes on the lines below the header")),
            "{:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // The same file with braces turned into `as ... end` parses, so
        // the spec builds.
        let good = "namespace A as\n    @test\n    function name()\n        $assert(1 == 1)\n    end\nend\n";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/good.aly"),
            good,
            None,
            &[],
        )
        .unwrap()
        .unwrap();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 1);
        assert!(text.contains("__lest.describe(\"good\""), "{text}");
    }
}
