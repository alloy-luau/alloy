//! Alloy compiler library.
//!
//! The pipeline is: parse Alloy, desugar to plain Luau, map every emitted
//! span back to its source span. This crate owns the desugar passes and the
//! position map. It does not own a type solver; luau-lsp checks the emitted
//! Luau.

pub mod alx;
pub mod build;
pub mod config;
pub mod declarations;
pub mod desugar;
pub mod directives;
pub mod docs;
pub mod extensions;
pub mod fmt;
pub mod lint;
pub mod luau_config;
pub mod project;
pub mod render;
pub mod roblox_classes;

pub use alx::{AlxOutput, compile_alx};
pub use desugar::{Diagnostic, EmitOptions, MacroSource};
pub use lint::Lint;
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

    let mut rendered = desugar::render(src, &parsed.lexed.toks, &parsed.chunk, options);

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

    diagnostics.extend(rendered.diagnostics);

    let mut lints = lint::run(src, &parsed.lexed.toks, &parsed.chunk, options.definitions);
    lints.extend(rendered.lints);
    lints.sort_by_key(|l| (l.start, l.name));
    // A node the desugar renders twice reports its lint twice.
    lints.dedup();

    // `--@alloy-nocheck` and `--@alloy-ignore` silence their lines.
    let silence = directives::scan(src);

    if !silence.is_empty() {
        diagnostics.retain(|d| silence.allows(directives::line_of(src, d.start as usize)));
        lints.retain(|l| silence.allows(directives::line_of(src, l.start as usize)));
    }

    // The ship artifact blanks type-only imports, keeping every position.
    let mut ship = rendered.text.clone().into_bytes();

    for (a, b) in &rendered.ship_blanks {
        for byte in ship.iter_mut().take(*b as usize).skip(*a as usize) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }

    Ok(Output {
        check,
        ship: String::from_utf8(ship).expect("blanking keeps UTF-8"),
        map: rendered.map,
        diagnostics,
        lints,
        uses_std: rendered.uses_std,
    })
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

    fn messages(src: &str) -> Vec<String> {
        compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
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
    fn an_impl_of_a_trait_writes_every_required_method() {
        let src = "trait Shape\n    function area(self): number\n    function scale(self, k: number): Shape\n    function name(self): string\n        return \"shape\"\n    end\nend\nstruct Sq as\n    s: number\nend\nimpl Shape for Sq\n    function scale(self): Shape\n        return self\n    end\nend\n";
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
        let bad = compile("local new = 1\nlocal function await() end\nlocal function f(delete) end\nprint(match)\n").unwrap();
        assert_eq!(bad.diagnostics.len(), 4, "{:?}", bad.diagnostics);
        assert!(bad.diagnostics[0].message.contains("reserved"));

        let fine = compile("struct V as\n    x: number\nend\nimpl V\n    function new(): V\n        return new V { x = 1 }\n    end\nend\nlocal make = Instance.new\nfunction V.await() end\nlocal v = new V()\nprint(make, v, V.new)\n").unwrap();
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
        let src = "struct Menu as\n    n: number\nend\nimpl Menu\n    function New(n: number): Menu\n        return new Menu { n = n }\n    end\nend\nlocal a = new Menu(1)\nlocal b = new Menu { n = 2 }\nlocal c = Menu(3)\nlocal d = Menu { n = 4 }\nprint(a, b, c, d)\n";
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
    fn nil_coalescing_desugars() {
        assert_eq!(
            desugar("local v = a ?? 0\n"),
            "local v = (if a == nil then 0 else a)\n"
        );
    }
}
