//! Alloy compiler library.
//!
//! The pipeline is: parse Alloy, desugar to plain Luau, map every emitted
//! span back to its source span. This crate owns the desugar passes and the
//! position map. It does not own a type solver; luau-lsp checks the emitted
//! Luau.

// `shapes.rs` names the crate as `alloy`, since the language server
// includes it the same way. Inside the crate that name needs an alias.
extern crate self as alloy;

pub mod alx;
pub mod build;
pub mod config;
pub mod data;
pub mod declarations;
pub mod desugar;
pub mod directives;
pub mod docs;
pub mod extensions;
pub mod flux;
pub mod fmt;
pub mod game_import;
pub mod globals;
pub mod ingot;
pub mod jsonc;
pub mod lint;
pub mod luau_config;
pub mod modules;
pub mod project;
pub mod render;
pub mod roblox_classes;
pub mod roblox_props;
pub mod roblox_services;
pub mod rojo;
pub mod schema;
/// The fold that turns the checker's printed types back into the names
/// the source wrote. The language server owns the file; the CLI reads
/// it so the terminal and the editor say the same thing.
#[allow(dead_code)]
#[path = "../../alloy-lsp/src/shapes/mod.rs"]
pub mod shapes;
pub mod tables;
pub mod testbuild;
pub mod typecheck;

pub use alx::{AlxOutput, compile_alx};
pub use desugar::{Diagnostic, EmitOptions, MacroSource, StructShape, WireField};
pub use lint::{Fix, Lint};
pub use render::SpanMap;

/// The markup compiler `.alx` files run through first; see crates/luaux.
pub use luaux;

/// The crate version, as set in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The output of one compile: the Luau text and its map.
///
/// The check artifact and the ship artifact are one text until a feature
/// needs them to differ; both fields exist so callers pick one on purpose.
pub struct Output {
    pub ship: String,
    pub check: String,
    pub map: SpanMap,
    pub diagnostics: Vec<Diagnostic>,
    /// Every lint that fired, at every level; `alloy lint` filters by
    /// the `[lint]` table.
    pub lints: Vec<Lint>,
    /// Whether the emitted code requires the runtime.
    pub uses_std: bool,
    /// The `import` statements: the path token's range and the path.
    pub imports: Vec<ImportRef>,
    /// Every `.json` or `.toml` path the source names in an `import`, an
    /// `import(...)`, or a `require(...)`, with the literal's range.
    pub data_refs: Vec<ImportRef>,
    /// The `@test` functions, in order: name and whether it is async.
    pub tests: Vec<(String, bool)>,
    /// The project globals the file named, each with the byte offset of
    /// its first use. The build reads them for the require graph.
    pub globals_used: Vec<(String, u32)>,
    /// Zero-based lines an `--@alloy-expect-error` covers that the
    /// compiler or a lint reported on.
    pub expected_hits: Vec<usize>,
    /// For `.alx`: the Alloy text luaux lowered the markup to. `map`
    /// already speaks the source, so this is what the lowering wrote,
    /// for a reader that wants to see it.
    pub lowered: Option<String>,
    /// Whether the parser read the whole file. A recovery invents the
    /// tree past the first error, so a lint or a type error over the
    /// emit describes code no one wrote.
    pub parsed_clean: bool,
}

/// One `import ... from "path"` of a file, or one data path with the
/// range of its literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRef {
    pub start: u32,
    pub end: u32,
    pub path: String,
}

/// A source that could not be lexed or parsed even leniently.
#[derive(Debug)]
pub struct CompileError {
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "byte {}: {}", self.offset, self.message)
    }
}

impl CompileError {
    /// The error the way every other diagnostic reads: `line:col:
    /// message`, a one-based line and a one-based column into `src`.
    /// `Display` cannot say this, since it holds no source; every
    /// printer that has the source calls this instead.
    pub fn located(&self, src: &str) -> String {
        let mut at = self.offset.min(src.len());

        while at > 0 && !src.is_char_boundary(at) {
            at -= 1;
        }

        let before = &src[..at];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, str::len) + 1;

        format!("{line}:{col}: {}", self.message)
    }
}

/// Compiles Alloy source to Luau.
///
/// Parse errors do not stop the compile: the lenient parser keeps the rest
/// of the file, and each error lands in `diagnostics` with its range. Only
/// a lex error fails, since the lexer decides what a token is.
pub fn compile(src: &str) -> Result<Output, CompileError> {
    compile_with(src, &EmitOptions::default())
}

/// The runtime module, shipped with the compiler. The build writes it to
/// the output root so the emitted `require` resolves.
pub const RUNTIME: &str = include_str!("../../std/alloy.luau");

/// The engine doubles `alloy test` loads before a spec; see
/// `std/shim.luau`.
pub const SHIM: &str = include_str!("../../std/shim.luau");

/// Compiles with the `[emit]` knobs of a project.
pub fn compile_with(src: &str, options: &EmitOptions) -> Result<Output, CompileError> {
    let parse_options = alloy_syntax::parser::ParseOptions {
        definitions: options.definitions,
        ..Default::default()
    };
    let parsed = alloy_syntax::parse_lenient(src, parse_options).map_err(|e| CompileError {
        offset: e.offset,
        message: e.message,
    })?;

    // The ship artifact reaches the runtime and every global module by
    // the instance path a mount gives it; the check artifact keeps the
    // file path. Only the specs differ, so one options clone carries both.
    let ship_globals: Vec<desugar::GlobalRef> = options
        .globals
        .iter()
        .map(|g| desugar::GlobalRef {
            require: g.ship_require.clone().unwrap_or_else(|| g.require.clone()),
            ..g.clone()
        })
        .collect();
    let ship_options = match options.ship_std_require.is_some()
        || options.globals.iter().any(|g| g.ship_require.is_some())
    {
        true => Some(EmitOptions {
            std_require: options
                .ship_std_require
                .clone()
                .unwrap_or_else(|| options.std_require.clone()),
            globals: ship_globals,
            ..options.clone()
        }),

        false => None,
    };
    let mut rendered = desugar::render(
        src,
        &parsed.lexed.toks,
        &parsed.chunk,
        ship_options.as_ref().unwrap_or(options),
    );

    // The check artifact is its own render: it types constructors and
    // `self`, casts what the checker cannot follow, and keeps `v:flat()`
    // for the analyzer. The map follows it, since the server is the
    // map's consumer.
    let mut check = rendered.text.clone();

    if !options.check {
        let check_options = EmitOptions {
            check: true,
            ..options.clone()
        };
        let second = desugar::render(src, &parsed.lexed.toks, &parsed.chunk, &check_options);
        check = second.text;
        rendered.map = second.map;
    }

    let mut diagnostics: Vec<Diagnostic> = parsed
        .diagnostics
        .iter()
        .map(|e| Diagnostic {
            start: e.offset as u32,
            end: e.offset as u32,
            message: e.message.clone(),
        })
        .collect();

    let parsed_clean = diagnostics.is_empty();
    diagnostics.extend(rendered.diagnostics);

    // A `global const` of another file is in scope here with no import,
    // so an assignment to one reads the same as one in its own file.
    let const_globals: Vec<(String, String)> = globals::used(src, &options.globals)
        .into_iter()
        .filter_map(|(name, _)| {
            // A name may be global on each side, so the one this file
            // reaches is the one whose `const` binds here.
            options
                .globals
                .iter()
                .find(|g| g.name == name && globals::reaches(g.side, options.side))
                .filter(|g| g.constant)
                .map(|g| (name, g.file.clone()))
        })
        .collect();

    for (start, end, message) in lint::const_reassignments(src, &parsed.lexed.toks, &const_globals)
    {
        diagnostics.push(Diagnostic {
            start,
            end,
            message,
        });
    }

    // A directive the compiler does not know silences nothing, so it
    // reads as a working one and is not. A directive it knows but
    // cannot accept reports the same way, on its own line.
    let scanned = directives::scan(src);

    for (line, message) in scanned
        .problems()
        .into_iter()
        .chain(scanned.side_problem(&options.file_name))
    {
        let (start, end) = directives::span_of_line(src, line);
        diagnostics.push(Diagnostic {
            start: start as u32,
            end: end as u32,
            message,
        });
    }

    // One mistake reaches the parser through several rules, so the same
    // sentence lands on one position more than once.
    diagnostics.sort_by_key(|d| d.start);
    diagnostics.dedup_by(|a, b| a.start == b.start && a.message == b.message);

    let mut lints = lint::run(
        src,
        &parsed.lexed.toks,
        &parsed.chunk,
        options.definitions,
        options.ingot_rewrite,
        &options.thresholds,
        &options.import_privates,
    );
    lints.extend(rendered.lints);

    // A file the parser reported on has a tree its recovery invented:
    // a statement lands in the block the parser could close, not the
    // one the author wrote, so a lint over it describes code no one
    // wrote. The parse error is the one thing to fix first.
    if !parsed_clean {
        lints.clear();
    }

    lints.sort_by_key(|l| (l.start, l.name));
    // A node the desugar renders twice reports its lint twice.
    lints.dedup();

    // `--@alloy-nocheck` and `--@alloy-ignore` silence their lines.
    // `--@alloy-expect-error` silences too, and remembers the lines it
    // covered that reported: the checker's pass adds its own and
    // reports each directive left over.
    let silence = scanned;
    let mut expected_hits = Vec::new();

    if !silence.is_empty() {
        for line in diagnostics
            .iter()
            .map(|d| directives::line_of(src, d.start as usize))
            .chain(
                lints
                    .iter()
                    // The lint that asks a directive for its reason is
                    // about the directive, not about the line it
                    // covers, so it meets no expectation.
                    .filter(|l| l.name != directives::MISSING_REASON)
                    .map(|l| directives::line_of(src, l.start as usize)),
            )
        {
            if silence.expects(line) && !expected_hits.contains(&line) {
                expected_hits.push(line);
            }
        }

        diagnostics.retain(|d| silence.allows(directives::line_of(src, d.start as usize)));
        lints.retain(|l| silence.allows_lint(directives::line_of(src, l.start as usize), l.name));
    }

    // A plain `require("./x.json")` passes through the desugar as it is;
    // both artifacts drop the extension so the require finds the module
    // the build writes. The literal only shrinks, so the map holds.
    let check = data::strip_requires(&check);

    // The ship artifact blanks type-only imports, keeping every position.
    let mut ship = data::strip_requires(&rendered.text).into_bytes();

    for (a, b) in &rendered.ship_blanks {
        for byte in ship.iter_mut().take(*b as usize).skip(*a as usize) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }

    let imports = parsed
        .chunk
        .block
        .stmts
        .iter()
        .filter_map(|s| match s {
            alloy_syntax::ast::Stmt::Import(i) => {
                let t = parsed.lexed.toks[i.path.start as usize];
                let text = t.text(src);
                let path = text
                    .get(1..text.len().saturating_sub(1))
                    .unwrap_or(text)
                    .to_string();

                Some(ImportRef {
                    start: t.start,
                    end: t.end,
                    path,
                })
            }

            _ => None,
        })
        .collect();

    Ok(Output {
        check,
        ship: String::from_utf8(ship).expect("blanking keeps UTF-8"),
        map: rendered.map,
        diagnostics,
        lints,
        uses_std: rendered.uses_std,
        imports,
        data_refs: data::references(src),
        tests: rendered.tests,
        globals_used: rendered.globals_used,
        expected_hits,
        lowered: None,
        parsed_clean,
    })
}

/// Compiles one file through the whole pipeline: the ingots' source
/// transforms, luaux for `.alx`, the desugar, then the ingots' lints and
/// output edits. Every caller that has a project goes through here, so
/// the build, the tests, and the editor agree on what a file becomes.
///
/// `path` picks the kind and is what the ingots see; `jsx` is the
/// project's markup config, its `[alx]` table, used for `.alx`.
pub fn compile_file(
    path: &str,
    source: &str,
    options: &EmitOptions,
    jsx: Option<&luaux::Config>,
    ingots: Option<&ingot::Ingots>,
) -> Result<Output, CompileError> {
    let ingots = ingots.filter(|i| !i.is_empty());
    let layer = ingots.map(|i| i.before(path, source));
    let text = layer.as_ref().map_or(source, |l| l.text.as_str());
    // An ingot's own statements stand where it put them, so a lint
    // about the order of the source has nothing to read.
    let rewritten = layer.as_ref().is_some_and(|l| l.text != source);
    let options = &match rewritten {
        true => EmitOptions {
            ingot_rewrite: true,
            ..options.clone()
        },

        false => options.clone(),
    };
    let mut out = if path.ends_with(".alx") {
        compile_alx(text, options, jsx.cloned().unwrap_or_default())?.output
    } else {
        compile_with(text, options)?
    };

    if let Some(layer) = layer {
        if let Some(map) = layer.map {
            // The built-once warning is about the author's markup. A
            // child an ingot wrapped in a call is built once by design,
            // and the author has no text there to change.
            out.diagnostics.retain(|d| {
                !(map.is_generated(d.start)
                    && d.message.starts_with("markup: this child is built once"))
            });

            // Everything the compile placed sits in the transformed text;
            // the author reads positions in their own.
            for d in &mut out.diagnostics {
                d.start = map.to_source(d.start);
                d.end = map.to_source(d.end).max(d.start);
            }

            // A lint is about the author's code: one inside the text an
            // ingot wrote has no line to point at.
            out.lints.retain(|l| !map.is_generated(l.start));

            for l in &mut out.lints {
                l.start = map.to_source(l.start);
                l.end = map.to_source(l.end).max(l.start);

                if let Some(f) = &mut l.fix {
                    // A fix over transformed bytes cannot apply to the
                    // author's text; keep the lint and drop the rewrite.
                    if map.is_generated(f.start) || map.is_generated(f.end.saturating_sub(1)) {
                        l.fix = None;
                    } else {
                        f.start = map.to_source(f.start);
                        f.end = map.to_source(f.end).max(f.start);
                    }
                }
            }

            for i in &mut out.imports {
                i.start = map.to_source(i.start);
                i.end = map.to_source(i.end).max(i.start);
            }

            out.map = map.compose(&out.map);
        }

        out.diagnostics.extend(layer.diagnostics);
        out.diagnostics.sort_by_key(|d| d.start);
    }

    if let Some(ingots) = ingots {
        ingots.after(path, source, &mut out);
    }

    Ok(out)
}

/// Desugars Alloy source to plain Luau, the ship artifact.
///
/// Every valid Luau file is a valid Alloy file, and a file that uses no
/// Alloy feature comes back byte for byte. The round-trip harness holds
/// that on the Luau conformance corpus.
#[must_use]
pub fn desugar(source: &str) -> String {
    match compile(source) {
        Ok(out) => out.ship,

        Err(_) => source.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reduce_accumulator_takes_the_literal_type() {
        let out = compile("local xs = [ 1, 2 ]\nlocal t = xs:reduce(function(acc, n) return acc + n end, 0)\nlocal s = xs:reduce(function(acc, n) return acc .. n end, \"\")\nprint(t, s)\n").unwrap();
        assert!(
            out.check.contains("function(acc: number, n)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("function(acc: string, n)"),
            "{}",
            out.check
        );
    }

    fn messages(src: &str) -> Vec<String> {
        compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    /// `alloy doc const` says a reassignment is a compile error. It
    /// used to reach the reader only through `alloy flux`, as a Luau
    /// syntax error.
    #[test]
    fn a_const_reassignment_is_a_compile_error() {
        let src = "const MAX = 3\nMAX = 4\nprint(MAX)\n";
        assert_eq!(
            messages(src),
            vec!["`MAX` is a `const`; its value is set once and a reassignment is an error"]
        );
        assert_eq!(docs::kind_for(&messages(src)[0]), "ConstError");
        // A compound assignment is one too, and a plain `local` is free.
        assert_eq!(messages("const MAX = 3\nMAX += 1\nprint(MAX)\n").len(), 1);
        assert!(messages("local max = 3\nmax = 4\nprint(max)\n").is_empty());
        // A read is not a write.
        assert!(messages("const MAX = 3\nlocal n = MAX + 1\nprint(n)\n").is_empty());
    }

    /// A statement under a `return` used to end the block early, so the
    /// function's own `end` read as a stray one and two syntax errors
    /// landed on the wrong lines.
    #[test]
    fn a_statement_after_a_return_gives_no_syntax_error() {
        let src =
            "local function later(n: number): number\n    return n\n    n = 1\nend\nreturn later\n";
        assert_eq!(messages(src), Vec::<String>::new());
        let out = compile(src).unwrap();
        assert!(out.ship.contains("do return n end"), "{}", out.ship);
        let names: Vec<&str> = out.lints.iter().map(|l| l.name).collect();
        assert!(names.contains(&"unreachable_code"), "{names:?}");
    }

    #[test]
    fn the_fields_form_names_every_field_without_a_default() {
        let src = "struct P as\n    x: number\n    y: number = 0\nend\nlocal a = new P { y = 1 }\nlocal b = new P { x = 1, z = 2 }\nlocal c = new P { x = 1 }\n";
        let got = messages(src);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("leaves `x` unset"), "{got:?}");
        assert!(got[1].contains("has no field `z`"), "{got:?}");
    }

    #[test]
    fn a_spread_turns_the_missing_field_check_off() {
        let src = "struct P as\n    x: number\nend\nlocal a = new P { x = 1 }\nlocal b = new P { ...a }\n";
        assert!(messages(src).is_empty());
    }

    #[test]
    fn a_compile_error_reads_as_a_position() {
        let src = "local a = 1\nlocal b = <TextLabel />\n";
        let e = CompileError {
            offset: src.find('<').unwrap(),
            message: "markup: `React` is not in scope".to_string(),
        };
        assert_eq!(e.located(src), "2:11: markup: `React` is not in scope");
        assert_eq!(
            CompileError {
                offset: 0,
                message: "boom".to_string(),
            }
            .located(""),
            "1:1: boom"
        );
    }

    #[test]
    fn an_impl_of_a_trait_writes_every_required_method() {
        let src = "trait Shape as\n    function area(self): number\n    function scale(self, k: number): Shape\n    function name(self): string\n        return \"shape\"\n    end\nend\nstruct Sq as\n    s: number\nend\nimpl Shape for Sq as\n    function scale(self): Shape\n        return self\n    end\nend\n";
        let got = messages(src);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            got.iter().any(|m| m.contains("does not write `area`")),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|m| m.contains("`scale` takes 2 parameters in `Shape`, 1 here")),
            "{got:?}"
        );
    }

    #[test]
    fn a_sealed_struct_guards_undeclared_writes() {
        let src = "@sealed\nstruct P as\n    x: number\nend\n";
        let out = compile(src).unwrap();
        assert!(out.diagnostics.is_empty());
        assert!(out.ship.contains(
            "P.__newindex = function(t, k, v) if ({ x = true })[k] then rawset(t, k, v) else error("
        ));
    }

    #[test]
    fn a_remote_rejects_a_type_that_cannot_cross_the_wire() {
        let src = "remote Ping(cb: () -> (), n: number, t: thread) from client\n";
        let got = messages(src);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("`cb` has type `() -> ()`, which is a function type"));
        assert!(got[1].contains("`t` has type `thread`, which is a coroutine"));
    }

    #[test]
    fn a_missing_variant_is_named() {
        let src = "enum Dir as\n    Up\n    Down\n    Left(number)\nend\nlocal d: Dir = Dir.Up\nmatch d with\n    case Up then print(1)\n    case Left(n) then print(n)\nend\n";
        let got = messages(src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].contains("`Dir` has no arm for `Down`; add it or a `default` arm"),
            "{got:?}"
        );
    }

    #[test]
    fn a_ternary_branch_may_call_a_method_on_a_call() {
        let out =
            compile("local level = 7\nlocal tier = level > 5 ? tostring(level):rep(2) : \"low\"\n")
                .unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("(if level > 5 then tostring(level):rep(2) else \"low\")"),
            "{}",
            out.check
        );
    }

    #[test]
    fn every_doc_key_renders_and_the_lints_have_docs() {
        for (key, text) in docs::TABLE {
            assert!(!text.is_empty(), "{key}");
        }

        for l in lint::LINTS {
            assert!(!l.detail.is_empty(), "{}", l.name);
        }

        assert!(docs::lookup("@sealed").is_some());
        assert!(docs::lookup("topic:strict").is_some());
    }

    #[test]
    fn delete_dispatches_through_the_runtime() {
        let out = compile("local conn = x\ndelete conn\n").unwrap();
        assert!(out.ship.contains("__alloy.delete(conn)"), "{}", out.ship);
        assert!(out.uses_std);
    }

    #[test]
    fn misplaced_attributes_are_diagnostics() {
        let out = compile(
            "@u16\nlocal y = 1\nlocal function g(@u16 x: number) return x end\nprint(y, g)\n",
        )
        .unwrap();
        assert_eq!(out.diagnostics.len(), 2, "{:?}", out.diagnostics);
        assert!(
            out.ship.lines().next().unwrap().trim().is_empty(),
            "{}",
            out.ship
        );
        assert!(out.ship.contains("local y = 1"));
    }

    #[test]
    fn a_declared_type_beats_the_ambient_one() {
        let out = compile("type Sink<T> = { [K in keyof T]: write T[K] }\nlocal a: Sink<{ x: number }> = { x = 1 }\nlocal b: Partial<{ x: number }> = {}\nprint(a, b)\n").unwrap();
        assert!(out.ship.contains("local a: Sink<"), "{}", out.ship);
        assert!(
            out.ship.contains("local b: __alloy.Partial<"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn reserved_words_cannot_be_names() {
        let bad = compile("local new = 1\nlocal function await() end\nlocal function f(delete) end\nprint(match)\nlocal private = 1\n").unwrap();
        assert_eq!(bad.diagnostics.len(), 5, "{:?}", bad.diagnostics);
        assert!(bad.diagnostics[0].message.contains("reserved"));

        let fine = compile("struct V as\n    x: number\nend\nimpl V as\n    function new(): V\n        return new V { x = 1 }\n    end\nend\nlocal make = Instance.new\nfunction V.await() end\nlocal v = new V()\nprint(make, v, V.new)\n").unwrap();
        assert!(fine.diagnostics.is_empty(), "{:?}", fine.diagnostics);
    }

    #[test]
    fn import_expression_is_require() {
        let out = compile("local m = import(\"./x\")\nlocal i = import(script.Parent.Mod)\nlocal d = import(paths[1])\nlocal t = import<<Config>>(name)\nprint(m, i, d, t)\n").unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("local m = require(\"./x\")"),
            "{}",
            out.ship
        );
        assert!(
            out.ship.contains("local i = require(script.Parent.Mod)"),
            "{}",
            out.ship
        );
        assert!(
            out.ship
                .contains("local d = (require(paths[1]) :: unknown)"),
            "{}",
            out.ship
        );
        assert!(
            out.ship.contains("local t = (require(name) :: Config)"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_written_constructor_is_the_way_in() {
        let src = "struct Menu as\n    n: number\nend\nimpl Menu as\n    function New(n: number): Menu\n        return new Menu { n = n }\n    end\nend\nlocal a = new Menu(1)\nlocal b = new Menu { n = 2 }\nlocal c = Menu(3)\nlocal d = Menu { n = 4 }\nprint(a, b, c, d)\n";
        let out = compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(messages[0].contains("writes `New`"), "{messages:?}");
        assert!(messages[1].contains("is not a call"), "{messages:?}");
        assert!(messages[2].contains("writes a constructor"), "{messages:?}");
        assert!(out.ship.contains("local a = Menu.New(1)"), "{}", out.ship);
        assert!(out.ship.contains("return Menu({ n = n })"), "{}", out.ship);
    }

    #[test]
    fn a_struct_without_new_takes_the_fields_form() {
        let src = "struct Box as\n    n: number\nend\nlocal a = new Box { n = 1 }\nlocal b = new Box({ n = 2 })\nlocal c = new Box(3)\nlocal d = new Box()\nlocal e = Box { n = 5 }\nprint(a, b, c, d, e)\n";
        let out = compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages.len(), 4, "{messages:?}");
        assert!(
            messages[..3]
                .iter()
                .all(|m| m.contains("writes no `new` or `New`")),
            "{messages:?}"
        );
        assert!(messages[3].contains("new Box { ... }"), "{messages:?}");
        assert!(
            out.ship.contains("local a = Box({ n = 1 })"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn ternary_keeps_a_fused_method_call() {
        let out = compile("local n = 3\nlocal s = n > 0 ? tostring(n):upper() : \"none\"\nlocal t = n > 5 ? \"a\" : n > 2 ? \"b\" : \"c\"\nprint(s, t)\n").unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship
                .contains("(if n > 0 then tostring(n):upper() else \"none\")"),
            "{}",
            out.ship
        );
        assert!(
            out.ship
                .contains("(if n > 5 then \"a\" else (if n > 2 then \"b\" else \"c\"))"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn calling_a_struct_is_a_diagnostic() {
        let out = compile("struct Vec2 as\n    x: number\nend\nlocal a = Vec2(1)\nlocal b = new Vec2 { x = 1 }\nprint(a, b)\n").unwrap();
        assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
        assert!(out.diagnostics[0].message.contains("new Vec2 { ... }"));
    }

    #[test]
    fn plain_luau_round_trips_unchanged() {
        let source = "local x = 1 -- comment\nprint(x)\n";
        assert_eq!(desugar(source), source);
    }

    #[test]
    fn a_wait_timeout_reaches_every_wait_for_child_and_guards_it() {
        let options = EmitOptions {
            wait_timeout: Some(5.0),
            ..EmitOptions::default()
        };
        let out = compile_with("local h = gui=>Hud=>Health\n", &options).unwrap();
        assert_eq!(
            out.ship,
            "local _1 = gui:WaitForChild(\"Hud\", 5) local h = (if _1 == nil then nil else _1:WaitForChild(\"Health\", 5))\n"
        );

        let out = compile("local h = gui=>Hud=>Health\n").unwrap();
        assert_eq!(
            out.ship,
            "local h = gui:WaitForChild(\"Hud\"):WaitForChild(\"Health\")\n"
        );
    }

    #[test]
    fn a_macro_local_never_captures_the_callers_name() {
        let src = "macro add_one(x)\n    local tmp = 1\n    x + tmp\nend\nlocal tmp = 10\nlocal r = $add_one(tmp)\nprint(r)\n";
        let out = compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("local tmp__m1 = 1 return tmp + tmp__m1"),
            "{}",
            out.ship
        );

        // Two expansions get two names; a loop variable and a parameter
        // rename too.
        let src = "macro twice(x)\n    for i = 1, 2 do\n        local f = function(k) return k + x end\n        print(f(i))\n    end\nend\n$twice(i)\n$twice(k)\n";
        let out = compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("i__m1") && out.ship.contains("k__m2"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn structs_and_enums_print_by_default_unless_the_author_writes_a_printer() {
        let src = "struct V as\n    x: number\n    y: number\nend\nenum Msg as\n    Move(number)\n    Quit\nend\nprint(new V { x = 1, y = 2 }, Msg.Move(1))\n";
        let out = compile(src).unwrap();
        assert!(
            out.ship.contains(
                "V.__tostring = function(s) return __alloy.show_struct(\"V\", s, { \"x\", \"y\" }) end"
            ),
            "{}",
            out.ship
        );
        assert!(
            out.ship.contains(
                "Msg.__tostring = function(v) return __alloy.show_variant(\"Msg\", v) end"
            ),
            "{}",
            out.ship
        );
        assert!(out.check.contains("function(s: V)"), "{}", out.check);

        let own = "struct V as\n    x: number\nend\nimpl V as\n    function to_string(self): string\n        return `v{self.x}`\n    end\nend\n@derive(Debug)\nstruct W as\n    x: number\nend\n";
        let out = compile(own).unwrap();
        assert!(!out.ship.contains("show_struct"), "{}", out.ship);
        assert!(
            out.ship
                .contains("W.__tostring = function(s) return \"W { \""),
            "{}",
            out.ship
        );
    }

    #[test]
    fn private_members_leave_the_public_view_of_the_check_artifact() {
        let src = "struct Counter as\n    read name: string\n    private count: number = 0\nend\nimpl Counter as\n    function bump(self): number\n        self.count += 1\n        self:log()\n        return self.count\n    end\n    public function peek(self): number\n        return self.count\n    end\n    private function log(self)\n        print(self.count)\n    end\nend\n";
        let out = compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // The ship artifact: every method on the class, no view.
        assert!(
            out.ship.contains("function Counter.log(self)"),
            "{}",
            out.ship
        );
        assert!(!out.ship.contains("private"), "{}", out.ship);
        assert!(!out.ship.contains("Counter__all"), "{}", out.ship);
        // The check artifact: the public type, the full view, the
        // private method on its own table, `self` rebound in public ones.
        assert!(
            out.check.contains("local Counter__private = {}"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("type Counter = typeof(setmetatable({} :: { read name: string }, Counter)) type Counter__all = Counter & { count: number } & typeof(Counter__private)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("function Counter.bump(self: Counter): number local self = (self :: any) :: Counter__all"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("function Counter.peek(self: Counter): number local self"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("function Counter__private.log(self: Counter__all)"),
            "{}",
            out.check
        );
        assert_eq!(out.check.lines().count(), src.lines().count());
    }

    #[test]
    fn a_module_and_names_import_in_one_line() {
        let out =
            compile("import jecs, { world, type Entity } from \"@pkg/jecs\"\nprint(jecs, world)\n")
                .unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // `jecs` is the module's default, and the named list reads the
        // module itself, so both sit on the hoisted require.
        assert!(
            out.ship.starts_with(
                "local _m1 = require(\"@pkg/jecs\") local jecs = _m1.default local world = _m1.world \
                 type Entity = _m1.Entity\n"
            ),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_half_typed_field_reports_once_and_the_struct_still_parses() {
        let src = "struct T as\n    public count: number\n    private \nend\nlocal t = new T { count = 1 }\nprint(t)\n";
        let out = compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            vec!["`private` needs a field name after it: `private name: T`"],
            "{messages:?}"
        );
        // The diagnostic sits on the modifier, not on the `end` below.
        assert_eq!(
            out.diagnostics[0].start as usize,
            src.find("private").unwrap()
        );
        assert!(
            out.ship.contains("local t = T({ count = 1 })"),
            "{}",
            out.ship
        );

        let src = "struct T as\n    name\n    x: number\nend\n";
        let out = compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            vec!["field `name` needs a type: `name: T`"],
            "{messages:?}"
        );
    }

    #[test]
    fn new_on_a_name_without_a_constructor_is_one_diagnostic_over_the_expression() {
        let src = "attribute icon(asset: string) on struct\nenum Msg as\n    Quit\nend\nlocal a = new icon { }\nlocal m = new Msg { }\nprint(a, m)\n";
        let out = compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].starts_with("`icon` is an attribute and cannot be constructed with `new`"),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with("`Msg` is an enum and cannot be constructed with `new`"),
            "{messages:?}"
        );
        let whole = &src[out.diagnostics[0].start as usize..out.diagnostics[0].end as usize];
        assert_eq!(whole, "new icon { }");
        assert_eq!(docs::kind_for(messages[0]), "ConstructorError");
    }

    #[test]
    fn an_imported_struct_constructs_through_the_runtime_and_types_in_the_check() {
        let src = "import { Counter } from \"./counter\"\nlocal c = new Counter {\n    name = \"hits\",\n}\nlocal d = new Counter(1) { name = \"x\" }\nprint(c, d)\n";
        let out = compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // The table copies as it is, lines and all: the runtime constructs
        // in the ship artifact, the typed constructor in the check one.
        assert!(
            out.ship
                .contains("local c = __alloy.construct(Counter, {\n    name = \"hits\",\n})"),
            "{}",
            out.ship
        );
        assert!(
            out.check
                .contains("local c = Counter.new({\n    name = \"hits\",\n})"),
            "{}",
            out.check
        );
        // With arguments the fields land one per line after the call.
        assert!(
            out.ship.contains("local d = Counter.new(1) d.name = \"x\""),
            "{}",
            out.ship
        );
        assert_eq!(out.ship.lines().count(), src.lines().count());
    }

    #[test]
    fn diagnostics_carry_a_kind() {
        assert_eq!(
            docs::kind_for("`new` is a reserved word and cannot be a name"),
            "ReservedWord"
        );
        assert_eq!(
            docs::kind_for("expected a name, found `end`"),
            "SyntaxError"
        );
        assert_eq!(
            docs::kind_for("this match is not exhaustive: `Msg` has no arm for `Leave`"),
            "ExhaustiveMatch"
        );
        assert_eq!(
            docs::labeled("internal: generated text holds a newline"),
            "InternalError: internal: generated text holds a newline"
        );
    }

    #[test]
    fn nil_coalescing_desugars() {
        assert_eq!(
            desugar("local v = a ?? 0\n"),
            "local v = (if a == nil then 0 else a)\n"
        );
    }
}
