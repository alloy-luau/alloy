//! The desugar pass: walks the tree and renders Luau.
//!
//! One walk serves every feature. A node the walk knows how to rewrite
//! renders generated text; every other node copies its source and recurses
//! into its children, so the text between children survives untouched.
//! Statements own the hoists their expressions need, and a block owns the
//! temp names its statements declare, so a temp is declared once per block
//! and assigned on later use. That keeps a long block under Luau's limit
//! of two hundred locals.
//!
//! The `Desugar` struct's inherent methods spread across the files in this
//! folder: this file holds the struct, the entry point, and the span and
//! hoist machinery every other file shares. Each sibling file holds one
//! seam of the walk (statements, expressions, types, structs, enums,
//! modules, remotes, attributes, macros).

use std::collections::HashMap;
use std::collections::HashSet;

use crate::lint::Lint;
use alloy_syntax::ast::{
    Binding, Block, CallArgs, ChildName, Chunk, ClassMember, Cond, DefaultExport, Destructure,
    Expr, FunctionBody, GenericFor, If, IndexKey, Local, Stmt, TableField, TokSpan, TypeEdit,
};
use alloy_syntax::lexer::Tok;

use crate::render::{NewlineInGenerated, Renderer, SpanMap};

mod attributes;
mod enums;
mod expressions;
mod macros;
mod modules;
mod remotes;
mod statements;
mod structs;
mod types;

use macros::MacroRef;
pub(crate) use remotes::WIRE_WIDTHS;

/// A message tied to a source byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub start: u32,
    pub end: u32,
    pub message: String,
}

/// The `[emit]` knobs, see `config::Emit`, plus what the compiler knows
/// about the file.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitOptions {
    pub wait_timeout: Option<f64>,
    /// The path shown in `$dbg`, `$todo`, and `$unreachable` messages.
    pub file_name: String,
    /// The string passed to `require` for the runtime.
    pub std_require: String,
    /// The ship artifact's runtime require when it differs: under a
    /// mount it is the instance path, while the check artifact and the
    /// tests keep the file path.
    pub ship_std_require: Option<String>,
    /// A `.d.aly`: declarations only, no runtime tables, no std require.
    pub definitions: bool,
    /// Blank `import type` lines in the ship artifact, so a type-only
    /// import creates no runtime dependency. Off by default, because the
    /// output is then untyped for anyone who analyzes it directly.
    pub erase_type_imports: bool,
    /// Macros visible to an expansion, as source: a nested compile of a
    /// macro body sees the macros of the file it came from.
    pub macros: Vec<MacroSource>,
    /// The structs of the whole project, for the wire layout of a
    /// remote that carries one from another file.
    pub shapes: Vec<StructShape>,
    /// Render the check artifact: a call to an extension method on a
    /// foreign type stays as written, and `self` in such an impl carries
    /// the target type, so the analyzer types both. The ship artifact
    /// routes the call through the dispatcher instead.
    pub check: bool,
    /// Extensions declared anywhere in the project, so a call by one of
    /// their names routes through the dispatcher in every file, not only
    /// in the file that declares the impl.
    pub extensions: Vec<crate::extensions::Extension>,
    /// The limits of the complexity lints.
    pub thresholds: crate::lint::Thresholds,
    /// Render the test artifact: a `@test` function stays in the output
    /// as a local, unregistered, for `alloy test` to call by name.
    pub tests: bool,
    /// Per import spec, the type names the module exports, so a value
    /// import of a struct or an enum binds the type too. See
    /// `crate::modules::import_types`.
    pub import_types: Vec<(String, Vec<String>)>,
    /// The enums the imported modules declare, with their variants and
    /// payload counts. A `match` over an imported enum covers it.
    /// See `crate::modules::import_enums`.
    pub import_enums: Vec<(String, Vec<(String, usize)>)>,
    /// Per imported trait, the names of its default methods, so an
    /// `impl Trait for S` here flattens them in as a local trait's would.
    pub import_trait_defaults: Vec<(String, Vec<String>)>,
    /// The imported async functions declared to return a `Result`: a
    /// `try await f()` on one is the Result itself. See
    /// `crate::modules::import_result_asyncs`.
    pub import_result_asyncs: Vec<String>,
    /// Per struct an imported module declares, its private field names,
    /// so `private_access` reports a read across a module boundary. See
    /// `crate::modules::import_privates`.
    pub import_privates: Vec<(String, Vec<String>)>,
    /// The specs that name a module Alloy does not compile: a `.luau`
    /// or `.lua` file, or a data file. Such a module has no export
    /// table, so `import X from` binds the value it returns, not
    /// `require(...).default`. See `crate::modules::plain_modules`.
    pub plain_modules: Vec<String>,
    /// The `global` declarations of the project, as this file reaches
    /// them. A file that names one gets the require and the binding on
    /// its first line. See `crate::globals`.
    pub globals: Vec<GlobalRef>,
    /// Whether the file compiles inside a project. `global` needs one:
    /// without alloy.toml there is no set of files to reach.
    pub in_project: bool,
    /// A `global` this file declares whose name a definitions file of
    /// the project already declares, with that file. Two declarations,
    /// no way to pick.
    pub ambient_clashes: Vec<(String, String)>,
    /// The `global macro` declarations of the project. A macro expands
    /// where it is written, so the declaration travels, not a require.
    pub global_macros: Vec<MacroSource>,
    /// The `global attribute` declarations of the project, by name.
    pub global_attributes: Vec<(String, AttrDecl)>,
    /// The file is a script whose globals the build hoisted into a
    /// module beside it. The declarations go, and the injected require
    /// brings the names back.
    pub hoist_globals: bool,
    /// The side the project's tree gives the file, for a name that
    /// says none. The module a script's globals moved into takes the
    /// script's side this way too.
    pub side: Option<crate::directives::Side>,
}

/// One `global` of the project, as the file being compiled reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalRef {
    pub name: String,
    /// The declaring file, as a message names it.
    pub file: String,
    /// The require spec that reaches the declaring module from here.
    pub require: String,
    /// The ship artifact's require when it differs: under a mount it is
    /// the instance path, while the check artifact keeps the file path.
    pub ship_require: Option<String>,
    /// Whether the name binds a value.
    pub value: bool,
    /// Whether the name is a type too, so the file needs an alias.
    pub ty: bool,
    /// The parameter list of a generic type, `<T>`.
    pub type_params: String,
    /// The side the declaring file sits on. A global of one side is out
    /// of scope on the other; a shared module reaches both.
    pub side: Option<crate::directives::Side>,
}

/// One field of a struct or an interface, as the prescan keeps it.
#[derive(Debug, Clone)]
struct FieldType {
    name: String,
    ty: TokSpan,
    private: bool,
}

/// One `attribute name(params) on targets` the file declares: the
/// targets it takes, and each parameter's name and type.
pub type AttrDecl = (Vec<String>, Vec<(String, Option<String>)>);

/// A struct another file declares, for the wire layout of a remote
/// that carries it: each field with its type text and its width.
#[derive(Debug, Clone, PartialEq)]
pub struct StructShape {
    pub name: String,
    pub fields: Vec<WireField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WireField {
    pub name: String,
    pub ty: String,
    pub width: Option<String>,
}

/// A macro as source text, for expansion in a nested compile.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroSource {
    pub name: String,
    pub params: Vec<String>,
    pub variadic: bool,
    pub body: String,
    pub tail: Option<String>,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            wait_timeout: None,
            file_name: "<input>".to_string(),
            std_require: "@alloy".to_string(),
            ship_std_require: None,
            definitions: false,
            erase_type_imports: false,
            macros: Vec::new(),
            shapes: Vec::new(),
            check: false,
            extensions: Vec::new(),
            thresholds: crate::lint::Thresholds::default(),
            tests: false,
            import_types: Vec::new(),
            import_enums: Vec::new(),
            import_trait_defaults: Vec::new(),
            import_result_asyncs: Vec::new(),
            import_privates: Vec::new(),
            plain_modules: Vec::new(),
            globals: Vec::new(),
            in_project: false,
            ambient_clashes: Vec::new(),
            global_macros: Vec::new(),
            global_attributes: Vec::new(),
            hoist_globals: false,
            side: None,
        }
    }
}

/// The result of one desugar: the Luau text, its map, and any diagnostics.
pub struct Rendered {
    pub text: String,
    pub map: SpanMap,
    pub diagnostics: Vec<Diagnostic>,
    pub lints: Vec<Lint>,
    /// Output byte ranges to blank in the ship artifact.
    pub ship_blanks: Vec<(u32, u32)>,
    /// Whether the file required the std.
    pub uses_std: bool,
    /// Whether the file declares an extension on a foreign type, so the
    /// check artifact differs from the ship artifact.
    pub ext_used: bool,
    /// The `@test` functions: name and whether it is async.
    pub tests: Vec<(String, bool)>,
    /// The project globals the file named, each with the byte offset of
    /// its first use. The build reads them for the require graph.
    pub globals_used: Vec<(String, u32)>,
}

/// What `--@alloy-side` says when it sits anywhere but over a global.
pub const SIDE_ON_GLOBAL: &str =
    "`--@alloy-side` sits above a global; `--@alloy-file-side` is the one for a whole file";

/// The std names that are ambient in Alloy source.
pub const AMBIENT: &[&str] = &[
    "Future",
    "Result",
    "Ok",
    "Err",
    "Array",
    "HashMap",
    "Set",
    "Symbol",
    "Attributes",
    "Signal",
    "Queue",
    "Heap",
    "Scope",
    "Iter",
];

/// The std type names that are ambient in a type position. The list
/// mirrors the ambient check in alloy-syntax's type parser, which marks
/// the same names for the runtime prefix.
pub const AMBIENT_TYPES: &[&str] = &[
    "Future",
    "Result",
    "Array",
    "HashMap",
    "Set",
    "Signal",
    "SignalConnection",
    "Signalish",
    "Partial",
    "Readonly",
    "Sink",
    "Queue",
    "Heap",
    "Scope",
    "Iter",
];

pub const PRIMITIVES: &[&str] = &[
    "boolean", "number", "string", "table", "function", "thread", "buffer", "vector", "userdata",
];

pub fn render(src: &str, toks: &[Tok], chunk: &Chunk, options: &EmitOptions) -> Rendered {
    let mut edits = chunk.type_edits.clone();
    // Outer edits first when two start at the same byte, so `T[][]`
    // renders as `Array<Array<T>>`.
    edits.sort_by_key(|e| match e {
        TypeEdit::ArraySuffix {
            modifier,
            operand,
            brackets,
        } => {
            let start = match modifier {
                Some(m) => toks[m.start as usize].start,

                None => toks[operand.start as usize].start,
            };

            (start, u32::MAX - toks[brackets.end as usize - 1].end)
        }

        TypeEdit::AmbientName(span) => (toks[span.start as usize].start, u32::MAX),

        TypeEdit::Mapped { table, .. } => (toks[table.start as usize].start, 0),
    });

    let mut d = Desugar {
        src,
        toks,
        options: options.clone(),
        r: Renderer::new(src),
        diagnostics: Vec::new(),
        lints: Vec::new(),
        hoists: Vec::new(),
        new_stmt_next: 0,
        import_next: 0,
        expected_generic: None,
        result_asyncs: options.import_result_asyncs.iter().cloned().collect(),
        inserts: Vec::new(),
        return_at: None,
        struct_field_types: HashMap::new(),
        struct_wire: HashMap::new(),
        temp_next: 0,
        declared: Vec::new(),
        no_hoist: 0,
        chain_anchor: 0,
        barrier: 0,
        type_edits: edits,
        scopes: vec![HashSet::new()],
        uses_std: false,
        globals_used: Vec::new(),
        // The name and the directive are the file's own word; the
        // caller's side is the tree's, which is the weakest.
        file_side: crate::directives::effective_side(src, &options.file_name).or(options.side),
        own_names: match options.hoist_globals {
            // A hoisted global lives in the module beside the script,
            // so the script reaches it the way every other file does.
            true => top_level_names(src, toks, chunk)
                .into_iter()
                .filter(|n| !options.globals.iter().any(|g| &g.name == n))
                .collect(),

            false => top_level_names(src, toks, chunk),
        },
        exports: Vec::new(),
        has_default_export: false,
        enums: HashMap::from([(
            "Result".to_string(),
            vec![("Ok".to_string(), 1), ("Err".to_string(), 1)],
        )]),
        attr_decls: HashMap::new(),
        imported_names: HashSet::new(),
        ret_types: Vec::new(),
        result_aliases: HashSet::new(),
        enum_decls: HashMap::new(),
        impl_methods: HashMap::new(),
        renames: Vec::new(),
        ship_blanks: Vec::new(),
        structs: HashSet::new(),
        struct_generics: HashMap::new(),
        structs_with_new: HashMap::new(),
        impl_target: None,
        declared_types: HashSet::new(),
        traits: HashMap::new(),
        trait_required: HashMap::new(),
        struct_fields: HashMap::new(),
        generic_types: HashSet::new(),
        struct_methods: HashMap::new(),
        impl_generics: HashMap::new(),
        trait_impl_targets: HashSet::new(),
        elem_bounds: Vec::new(),
        ext_methods: HashSet::new(),
        ext_statics: HashMap::new(),
        self_type: None,
        ext_primitive: HashMap::new(),
        ext_hit: false,
        macros: HashMap::new(),
        test_names: Vec::new(),
        macro_serial: 0,
        structs_with_to_string: HashSet::new(),
        private_types: HashSet::new(),
        self_prologue: None,
        not_constructible: HashMap::new(),
        mapped_used: Vec::new(),
    };

    // A global macro is in scope before the file's own declarations, so
    // a file that declares the same name wins over the project's.
    for m in &options.global_macros {
        d.macros.insert(
            m.name.clone(),
            MacroRef {
                params: m.params.clone(),
                variadic: m.variadic,
                body: m.body.clone(),
                tail: m.tail.clone(),
            },
        );
    }

    for (name, decl) in &options.global_attributes {
        d.attr_decls.insert(name.clone(), decl.clone());
    }

    for m in &options.macros {
        d.macros.insert(
            m.name.clone(),
            MacroRef {
                params: m.params.clone(),
                variadic: m.variadic,
                body: m.body.clone(),
                tail: m.tail.clone(),
            },
        );
    }

    // A type slot names a global the same way a value does. The parser
    // recorded every bare type name; the ones a global owns need the
    // alias the first line writes.
    if !options.globals.is_empty() {
        for span in &chunk.type_names {
            let name = d.text_of(*span).to_string();
            let at = d.byte_start(*span);
            d.use_global(&name, at);
        }
    }

    // What each `global` of this file needs to be true. The check runs
    // before the walk, so a file with a bad global still renders.
    d.check_globals(src, toks, chunk);

    // Names that later statements route through, gathered up front.
    d.prescan(&chunk.block);
    d.scan_reduce_inserts(&chunk.block);
    d.scan_static_checks(&chunk.block);

    // Leading trivia, the block, trailing trivia: the printer's shape. The
    // std require, when the file needs one, goes on the first line after
    // the hot comments, so `--!strict` stays first.
    let insert_at = first_code_line(src) as u32;
    d.copy(0, insert_at);

    let mut side = Renderer::new(src);
    std::mem::swap(&mut d.r, &mut side);

    match toks.first() {
        Some(first) => {
            let first_start = first.start.max(insert_at);
            d.copy(insert_at, first_start);
            d.block(&chunk.block);
            let last = toks[toks.len() - 1].end;
            d.return_at = Some(d.r.out_len());
            d.module_return(last, &chunk.block);

            if d.r.out_len() == d.return_at.unwrap_or(0) {
                d.return_at = None;
            }

            d.copy(last, src.len() as u32);
        }

        None => d.copy(insert_at, src.len() as u32),
    }

    std::mem::swap(&mut d.r, &mut side);

    let mut prefix_len = 0u32;

    if d.uses_std && !options.definitions {
        let line = format!(
            "local __alloy = require({}) ",
            luau_string(&options.std_require)
        );
        prefix_len = line.len() as u32;
        d.generate(insert_at, &line);
    }

    // Every global the file named: the require of its module, then the
    // binding. The text holds no newline, so the line count stands.
    let global_line = d.global_prologue();

    if !global_line.is_empty() {
        d.generate(insert_at, &global_line);
    }

    for kind in d.mapped_used.clone() {
        let line = format!("{} ", Desugar::mapped_type_function(kind));
        d.generate(insert_at, &line);
    }

    let _ = prefix_len;
    // The export return follows the last statement; a blanked test at
    // the end of the file must not take it along.
    let return_start = d.return_at.map(|at| d.r.out_len() + at);
    d.r.append(side);

    let blanks = d.ship_blanks.clone();
    let (text, map) = d.r.finish();

    // Turn source ranges into output ranges through the map.
    let mut out_blanks = Vec::new();

    for (i, chunk) in map.chunks().iter().enumerate() {
        let (src_at, len) = match chunk {
            crate::render::Chunk::Copied { src_start, src_end } => {
                (*src_start, src_end - src_start)
            }

            crate::render::Chunk::Generated { anchor, len } => (*anchor, *len),
        };

        // A generated tail anchors at the end of its statement, so the end
        // is inclusive for generated text.
        let generated = matches!(chunk, crate::render::Chunk::Generated { .. });

        let start = map.chunk_start(i);

        if return_start.is_some_and(|r| start >= r) {
            continue;
        }

        if blanks
            .iter()
            .any(|(a, b)| src_at >= *a && (src_at < *b || (generated && src_at == *b)))
        {
            out_blanks.push((start, start + len));
        }
    }

    Rendered {
        text,
        map,
        diagnostics: d.diagnostics,
        lints: d.lints,
        ship_blanks: out_blanks,
        uses_std: d.uses_std,
        ext_used: d.ext_hit,
        tests: d.test_names,
        globals_used: d.globals_used,
    }
}

/// The names one top-level statement binds.
pub fn bound_names(src: &str, toks: &[Tok], stmt: &Stmt) -> Vec<String> {
    let text = |span: TokSpan| -> String {
        if span.end <= span.start || span.end as usize > toks.len() {
            return String::new();
        }

        src[toks[span.start as usize].start as usize..toks[span.end as usize - 1].end as usize]
            .to_string()
    };

    match stmt {
        Stmt::Local(l) => l.names.iter().map(|b| text(b.name)).collect(),

        Stmt::Function(f) => f.path.first().map(|n| text(*n)).into_iter().collect(),

        Stmt::LocalFunction(f) => vec![text(f.name)],

        Stmt::Struct(d) => vec![text(d.name)],

        Stmt::Enum(d) => vec![text(d.name)],

        Stmt::Trait(d) => vec![text(d.name)],

        Stmt::Interface(d) => vec![text(d.name)],

        Stmt::Class(d) => vec![text(d.name)],

        Stmt::TypeAlias(d) => vec![text(d.name)],

        Stmt::Remote(d) => vec![text(d.name)],

        Stmt::Macro(d) => vec![text(d.name)],

        Stmt::Attribute(d) => vec![text(d.name)],

        Stmt::Import(i) => import_names(i).into_iter().map(text).collect(),

        _ => Vec::new(),
    }
}

/// Every name the top level of a file binds: a local, a function, a
/// declaration, an import. A global by one of these names is the file's
/// own name, so nothing is injected over it.
fn top_level_names(src: &str, toks: &[Tok], chunk: &Chunk) -> HashSet<String> {
    let text = |span: TokSpan| -> String {
        if span.end <= span.start || span.end as usize > toks.len() {
            return String::new();
        }

        src[toks[span.start as usize].start as usize..toks[span.end as usize - 1].end as usize]
            .to_string()
    };
    let mut out = HashSet::new();

    for stmt in &chunk.block.stmts {
        match stmt {
            Stmt::Local(l) => {
                for b in &l.names {
                    out.insert(text(b.name));
                }
            }

            Stmt::Function(f) => {
                if let Some(first) = f.path.first() {
                    out.insert(text(*first));
                }
            }

            Stmt::LocalFunction(f) => {
                out.insert(text(f.name));
            }

            Stmt::Struct(d) => {
                out.insert(text(d.name));
            }

            Stmt::Enum(d) => {
                out.insert(text(d.name));
            }

            Stmt::Trait(d) => {
                out.insert(text(d.name));
            }

            Stmt::Interface(d) => {
                out.insert(text(d.name));
            }

            Stmt::Class(d) => {
                out.insert(text(d.name));
            }

            Stmt::TypeAlias(d) => {
                out.insert(text(d.name));
            }

            Stmt::Remote(d) => {
                out.insert(text(d.name));
            }

            Stmt::Macro(d) => {
                out.insert(text(d.name));
            }

            Stmt::Attribute(d) => {
                out.insert(text(d.name));
            }

            Stmt::Import(i) => {
                for name in import_names(i) {
                    out.insert(text(name));
                }
            }

            _ => {}
        }
    }

    out
}

/// Every name an `import` binds.
fn import_names(i: &alloy_syntax::ast::Import) -> Vec<TokSpan> {
    use alloy_syntax::ast::ImportKind;

    match &i.kind {
        ImportKind::Namespace(n) | ImportKind::Default(n) => vec![*n],

        ImportKind::Both(n, specs) => std::iter::once(*n)
            .chain(specs.iter().map(|s| s.alias.unwrap_or(s.name)))
            .collect(),

        ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
            specs.iter().map(|s| s.alias.unwrap_or(s.name)).collect()
        }
    }
}

/// The byte offset of the first line that is not a `--!` hot comment.
fn first_code_line(src: &str) -> usize {
    let mut at = 0;

    for line in src.split_inclusive('\n') {
        if line.starts_with("--!") {
            at += line.len();
        } else {
            break;
        }
    }

    at
}

/// One thing a statement hoists in front of itself.
enum Hoist<'s> {
    /// `local _k = value`, or `_k = value` when the block declared it.
    Temp {
        index: u32,
        value: HoistValue<'s>,
        anchor: u32,
    },
    /// A whole statement, such as the early return of `try`.
    Stmt { text: String, anchor: u32 },
    /// `local name = value`, always a new local: an import's module,
    /// whose type must not be another module's.
    Fresh {
        name: String,
        value: String,
        anchor: u32,
    },
}

/// What a temp holds: text the desugar wrote, or an expression rendered
/// with its provenance, so a receiver that spans lines keeps them.
enum HoistValue<'s> {
    Text(String),
    Rendered(Renderer<'s>),
}

struct Desugar<'s> {
    src: &'s str,
    toks: &'s [Tok],
    options: EmitOptions,
    r: Renderer<'s>,
    diagnostics: Vec<Diagnostic>,
    /// The lints the walk finds, see `crate::lint`.
    lints: Vec<Lint>,
    /// The hoists the statement under render has asked for, in order.
    hoists: Vec<Hoist<'s>>,
    /// The count of `new X(...) { }` statements, for their locals.
    new_stmt_next: u32,
    /// The count of imports, for their locals `_m1`, `_m2`, each its own.
    import_next: u32,
    /// `local m: HashMap<K, V> = HashMap.new()`: the annotation's base
    /// name and arguments, so the constructor call takes them. The
    /// solver reads no expected type into a generic call.
    expected_generic: Option<(String, String)>,
    /// The async functions of this file, and the imported ones, declared
    /// to return a `Result`: `try await` on a call to one is the Result.
    result_asyncs: HashSet<String>,
    /// Text to write at a source byte the next copy spans: the
    /// accumulator of a `reduce` takes the type of a literal initial
    /// value, since the checker reads the function before the value.
    inserts: Vec<(u32, String)>,
    /// Where the export return starts in the side buffer, once written.
    return_at: Option<u32>,
    /// The fields of each struct declared here with their types, for the
    /// mapped types the check artifact expands inline.
    struct_field_types: HashMap<String, Vec<FieldType>>,
    /// The fields of each struct declared here with their widths, for
    /// the wire layout of a remote.
    struct_wire: HashMap<String, Vec<WireField>>,
    /// The next temp index inside the statement under render.
    temp_next: u32,
    /// Per open block: the temp indices already declared in it, and in
    /// every block around it, since an inner block sees outer locals.
    declared: Vec<Vec<u32>>,
    /// Above zero while rendering a condition the loop re-evaluates. A
    /// temp hoisted before the statement would run once, not per pass.
    no_hoist: u32,
    /// Where the chain under render starts; the anchor of its hoists.
    chain_anchor: u32,
    /// The index into `declared` where the innermost function body starts.
    /// A temp is a local of its own function and never an upvalue, since
    /// two coroutines in one function must not share it.
    barrier: usize,
    /// Alloy syntax inside type spans, sorted by start.
    type_edits: Vec<TypeEdit>,
    /// Local names in scope, per block, for the ambient std names.
    scopes: Vec<HashSet<String>>,
    /// Whether the file needs the std require.
    uses_std: bool,
    /// The project globals this file named, first use first. Each one
    /// puts a require and a binding on the first line.
    globals_used: Vec<(String, u32)>,
    /// The side this file sits on, from its name or its directive.
    file_side: Option<crate::directives::Side>,
    /// Every name the top level of this file binds. A file that
    /// declares a name of its own keeps it; no global is injected over it.
    own_names: HashSet<String>,
    /// Names the module exports, as `name = value` pairs for the table.
    exports: Vec<(String, String)>,
    /// `export default` was seen.
    has_default_export: bool,
    /// Declared enums: name to (variant, payload count) list.
    enums: HashMap<String, Vec<(String, usize)>>,
    /// Attributes the file declares: name to (targets, parameter names
    /// and types). An attribute check reads it beside the built-in list.
    attr_decls: HashMap<String, AttrDecl>,
    /// Names an `import` brings in. An attribute of an imported name is
    /// not checked here; this file cannot see its targets.
    imported_names: HashSet<String>,
    /// The declared return type of each function body under render,
    /// innermost last. `try` reads it: it compiles only inside a function
    /// that returns a Result.
    ret_types: Vec<Option<String>>,
    /// Type aliases the file declares whose value is a `Result`, so a
    /// function that returns one still takes `try`.
    result_aliases: HashSet<String>,
    /// The enums this file declares, from the prescan, so a use before
    /// the declaration still checks. `enums` also holds `Result`, which
    /// the std owns and whose table carries more than its variants.
    enum_decls: HashMap<String, Vec<(String, usize)>>,
    /// Method and static names an `impl` block writes, by target. An enum
    /// member check reads it so a method call is not a missing variant.
    impl_methods: HashMap<String, HashSet<String>>,
    /// Pattern bindings under substitution in expression arms, innermost
    /// last: a binding name maps to the access path it stands for.
    renames: Vec<HashMap<String, String>>,
    /// Source ranges whose output the ship artifact blanks: type-only imports.
    ship_blanks: Vec<(u32, u32)>,
    /// Declared struct names, for pattern tests and `is`.
    structs: HashSet<String>,
    /// A struct's type parameters as the source writes them, `<T>`, for
    /// the structs that take any.
    struct_generics: HashMap<String, String>,
    /// The structs whose `impl` writes a constructor, `new` or `New`, by
    /// its name: they construct through it, and the fields form stays
    /// inside their own impl.
    structs_with_new: HashMap<String, String>,
    /// The struct whose `impl` renders now, whose own raw constructor is
    /// the constructor's business.
    impl_target: Option<String>,
    /// Type names the file declares at the top level, so an ambient type
    /// of the same name yields to them anywhere in the file.
    declared_types: HashSet<String>,
    /// Declared trait names with their default-method names.
    traits: HashMap<String, Vec<String>>,
    /// Declared trait names with the methods an impl must write: name,
    /// parameter count with `self` included, and the return type the
    /// signature declares.
    trait_required: HashMap<String, Vec<(String, usize, Option<String>)>>,
    /// Declared struct fields by struct name: field name and whether it
    /// carries a default.
    struct_fields: HashMap<String, Vec<(String, bool)>>,
    /// Structs declared with type parameters: their alias needs
    /// arguments, so the check artifact leaves `self` untyped there.
    generic_types: HashSet<String>,
    /// The instance methods of each struct an `impl` block writes, and
    /// the generic list of that block. The check artifact spells them
    /// into a generic struct's alias.
    struct_methods: HashMap<String, Vec<MethodSig>>,
    /// The generic list each `impl` block declares, by target.
    impl_generics: HashMap<String, String>,
    /// Structs a `impl Trait for` block targets. Their methods come from
    /// the trait too, so the alias cannot list them all.
    trait_impl_targets: HashSet<String>,
    /// Parameters of the function under render whose type is a bounded
    /// `T[]`: the name, and the `(T & Bound)` an element reads back as.
    /// Luau's Array is invariant, so the bound has to return at each
    /// element read.
    elem_bounds: Vec<(String, String)>,
    /// Extension method names declared on foreign types in this file, so
    /// `x:name(...)` routes through the dispatcher.
    ext_methods: HashSet<String>,
    /// Foreign types with statics declared on them: `Vector3.zero()`.
    ext_statics: HashMap<String, HashSet<String>>,
    /// The type an untyped `self` parameter gets while a foreign impl
    /// renders for the check artifact.
    self_type: Option<String>,
    /// Extension method name to its primitive target. A primitive has no
    /// class block to extend, so the check artifact calls a declared
    /// helper table, `__alloy_string.trim(s)`, instead. `None` marks a
    /// name two primitives both declare: the target follows the value,
    /// which only the run knows.
    ext_primitive: HashMap<String, Option<String>>,
    /// Whether the emit touched an extension: a foreign impl or a call by
    /// an extension name. Then the check artifact differs from the ship.
    ext_hit: bool,
    /// Macros declared in this file, by name.
    macros: HashMap<String, MacroRef>,
    /// The `@test` functions, with whether each is async. The check
    /// artifact registers them, the ship artifact blanks them, and the
    /// test artifact keeps them for `alloy test`.
    test_names: Vec<(String, bool)>,
    /// Mapped-type shapes used, each needing one type function declared.
    mapped_used: Vec<&'static str>,
    /// Expansions so far, for the unique names of a body's locals.
    macro_serial: u32,
    /// Structs whose impl writes `to_string`: they print through it, so
    /// the default printer stays out.
    structs_with_to_string: HashSet<String>,
    /// Structs with a `private` field or method. The check artifact
    /// gives each two views: the public type, and `Name__all` for the
    /// impl's own methods.
    private_types: HashSet<String>,
    /// A line the next function header runs first: the `self` of a
    /// public method rebound to the full view.
    self_prologue: Option<String>,
    /// Attributes, interfaces, and remotes declared in this file: names
    /// `new` cannot construct.
    not_constructible: HashMap<String, &'static str>,
}

/// One instance method an `impl` block writes, in spans, so the type
/// text lowers when the struct renders and not while the file is
/// scanned.
struct MethodSig {
    name: TokSpan,
    /// The generic list the method itself declares, `<U>`.
    generics: Option<TokSpan>,
    /// Every parameter after `self`: the name, the type, whether it is a
    /// vararg, and whether it carries a default.
    params: Vec<(TokSpan, Option<TokSpan>, bool, bool)>,
    ret: Option<TokSpan>,
}

fn if_has_local(i: &If) -> bool {
    i.branches
        .iter()
        .any(|(c, _)| matches!(c, Cond::Local { .. }))
}

/// One step of a postfix chain, without its optionality.
pub(crate) enum Step<'a> {
    Field(TokSpan),
    Computed(&'a Expr),
    Call {
        method: Option<TokSpan>,
        type_args: Option<TokSpan>,
        args: &'a CallArgs,
    },
    Child {
        name: &'a ChildName,
        wait: bool,
    },
}

pub(crate) enum Link<'a> {
    Plain(Step<'a>),
    Optional(Step<'a>),
    NonNil { span: TokSpan },
}

/// Splits a chain into its base and its links in source order.
fn flatten(e: &Expr) -> (&Expr, Vec<Link<'_>>) {
    let mut links = Vec::new();
    let mut cur = e;

    loop {
        match cur {
            Expr::Index {
                object,
                key,
                optional,
                ..
            } => {
                let step = match key {
                    IndexKey::Field(n) => Step::Field(*n),

                    IndexKey::Computed(k) => Step::Computed(k),
                };
                links.push(if *optional {
                    Link::Optional(step)
                } else {
                    Link::Plain(step)
                });
                cur = object;
            }

            Expr::Call {
                func,
                method,
                type_args,
                args,
                optional,
                ..
            } => {
                let step = Step::Call {
                    method: *method,
                    type_args: *type_args,
                    args,
                };
                links.push(if *optional {
                    Link::Optional(step)
                } else {
                    Link::Plain(step)
                });
                cur = func;
            }

            Expr::Child {
                object, name, wait, ..
            } => {
                let step = Step::Child { name, wait: *wait };
                links.push(if *wait {
                    Link::Plain(step)
                } else {
                    Link::Optional(step)
                });
                cur = object;
            }

            Expr::NonNil { operand, .. } => {
                links.push(Link::NonNil {
                    span: operand.span(),
                });
                cur = operand;
            }

            _ => break,
        }
    }

    links.reverse();

    (cur, links)
}

fn chain_has_alloy(e: &Expr) -> bool {
    let (base, links) = flatten(e);

    if links.is_empty() {
        return false;
    }

    matches!(
        base,
        Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. }
    ) || links.iter().any(|l| {
        matches!(
            l,
            Link::Optional(_) | Link::NonNil { .. } | Link::Plain(Step::Child { .. })
        )
    })
}

/// A number literal without a trailing `.0` for whole values.
fn luau_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A Luau string literal for arbitrary text.
fn luau_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');

    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),

            '\\' => out.push_str("\\\\"),

            '\n' => out.push_str("\\n"),

            '\r' => out.push_str("\\r"),

            '\t' => out.push_str("\\t"),

            c => out.push(c),
        }
    }

    out.push('"');

    out
}

/// A child of a node, in source order.
enum Child<'a> {
    Expr(&'a Expr),
    Block(&'a Block),
    /// A function body, which starts a fresh temp scope.
    Function(&'a FunctionBody),
}

impl Child<'_> {
    fn span(&self) -> TokSpan {
        match self {
            Child::Expr(e) => e.span(),

            Child::Block(b) => b.span,

            Child::Function(f) => f.block.span,
        }
    }
}

fn field_children(f: &TableField) -> Vec<Child<'_>> {
    match f {
        TableField::Positional(v) => vec![Child::Expr(v)],

        TableField::Named { value, .. } => vec![Child::Expr(value)],

        TableField::Computed { key, value } => vec![Child::Expr(key), Child::Expr(value)],

        TableField::Spread(v) => vec![Child::Expr(v)],
    }
}

fn cond_children(c: &Cond) -> Vec<Child<'_>> {
    match c {
        Cond::Expr(e) => vec![Child::Expr(e)],

        Cond::Local {
            bindings, filter, ..
        } => {
            let mut v: Vec<Child<'_>> = bindings.iter().map(|b| Child::Expr(&b.value)).collect();

            if let Some(f) = filter {
                v.push(Child::Expr(f));
            }

            v
        }
    }
}

fn function_children(body: &FunctionBody) -> Vec<Child<'_>> {
    let mut v: Vec<Child<'_>> = body
        .params
        .iter()
        .filter_map(|p| p.default.as_ref().map(Child::Expr))
        .collect();
    v.push(Child::Function(body));

    v
}

fn expr_children(e: &Expr) -> Vec<Child<'_>> {
    match e {
        Expr::Nil(_)
        | Expr::True(_)
        | Expr::False(_)
        | Expr::Vararg(_)
        | Expr::Number(_)
        | Expr::String(_)
        | Expr::InterpString(_)
        | Expr::Name(_) => Vec::new(),

        Expr::Interp { parts, .. } => parts.iter().map(Child::Expr).collect(),

        Expr::Function { body, .. } => function_children(body),

        Expr::Table { fields, .. } => fields.iter().flat_map(field_children).collect(),

        Expr::Binary { lhs, rhs, .. } => vec![Child::Expr(lhs), Child::Expr(rhs)],

        Expr::Unary { operand, .. } => vec![Child::Expr(operand)],

        Expr::Paren { inner, .. } => vec![Child::Expr(inner)],

        Expr::Index { object, key, .. } => {
            let mut v = vec![Child::Expr(object)];

            if let IndexKey::Computed(k) = key {
                v.push(Child::Expr(k));
            }

            v
        }

        Expr::Call { func, args, .. } => {
            let mut v = vec![Child::Expr(func)];

            match args {
                CallArgs::Paren(list) => v.extend(list.iter().map(Child::Expr)),

                CallArgs::Table(t) => v.push(Child::Expr(t)),

                CallArgs::Str(_) => {}
            }

            v
        }

        Expr::IfElse {
            branches,
            else_value,
            ..
        } => {
            let mut v = Vec::new();

            for (c, val) in branches {
                v.extend(cond_children(c));
                v.push(Child::Expr(val));
            }

            v.push(Child::Expr(else_value));

            v
        }

        Expr::Match(m) => {
            let mut v: Vec<Child<'_>> = m.scrutinees.iter().map(Child::Expr).collect();

            for a in &m.arms {
                if let Some(g) = &a.guard {
                    v.push(Child::Expr(g));
                }

                v.push(Child::Expr(&a.value));
            }

            if let Some(d) = &m.default {
                v.push(Child::Expr(d));
            }

            v
        }

        Expr::TypeAssert { expr, .. } | Expr::Satisfies { expr, .. } | Expr::Is { expr, .. } => {
            vec![Child::Expr(expr)]
        }

        Expr::Child { object, name, .. } => {
            let mut v = vec![Child::Expr(object)];

            if let ChildName::Computed(k) = name {
                v.push(Child::Expr(k));
            }

            v
        }

        Expr::NonNil { operand, .. } | Expr::Await { operand, .. } | Expr::Try { operand, .. } => {
            vec![Child::Expr(operand)]
        }

        Expr::Ternary {
            cond,
            then_value,
            else_value,
            ..
        } => vec![
            Child::Expr(cond),
            Child::Expr(then_value),
            Child::Expr(else_value),
        ],

        Expr::Array { items, .. } => items.iter().map(Child::Expr).collect(),

        Expr::MethodRef { object, .. } => vec![Child::Expr(object)],

        Expr::New {
            name, args, init, ..
        } => {
            let mut v = vec![Child::Expr(name)];

            match args {
                Some(CallArgs::Paren(list)) => v.extend(list.iter().map(Child::Expr)),

                Some(CallArgs::Table(t)) => v.push(Child::Expr(t)),

                _ => {}
            }

            if let Some(i) = init {
                v.push(Child::Expr(i));
            }

            v
        }

        Expr::AsyncBlock { block, .. } | Expr::TryBlock { block, .. } => vec![Child::Block(block)],

        Expr::Macro { args, .. } => args.iter().map(Child::Expr).collect(),
    }
}

fn stmt_children(s: &Stmt) -> Vec<Child<'_>> {
    match s {
        Stmt::Empty(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::TypeAlias(_)
        | Stmt::Declare(_)
        | Stmt::Error(_) => Vec::new(),

        Stmt::Local(l) => l.values.iter().map(Child::Expr).collect(),

        Stmt::Assign(a) => a
            .targets
            .iter()
            .chain(a.values.iter())
            .map(Child::Expr)
            .collect(),

        Stmt::Call(e, _) | Stmt::Delete { expr: e, .. } => vec![Child::Expr(e)],

        Stmt::Do(d) => vec![Child::Block(&d.block)],

        Stmt::While(w) => {
            let mut v = cond_children(&w.cond);
            v.push(Child::Block(&w.block));

            v
        }

        Stmt::Repeat(r) => vec![Child::Block(&r.block), Child::Expr(&r.cond)],

        Stmt::If(i) => {
            let mut v = Vec::new();

            for (c, b) in &i.branches {
                v.extend(cond_children(c));
                v.push(Child::Block(b));
            }

            if let Some(b) = &i.else_block {
                v.push(Child::Block(b));
            }

            v
        }

        Stmt::Import(_) | Stmt::ExportList(_) => Vec::new(),

        Stmt::ExportDefault { value, .. } => match value {
            DefaultExport::Value(e) => vec![Child::Expr(e)],

            DefaultExport::Decl(inner) => stmt_children(inner),
        },

        Stmt::Enum(e) => e
            .variants
            .iter()
            .filter_map(|v| v.value.as_ref().map(Child::Expr))
            .collect(),

        Stmt::Impl(i) => i
            .methods
            .iter()
            .flat_map(|f| function_children(&f.body))
            .collect(),

        Stmt::Match(m) => {
            let mut v: Vec<Child<'_>> = m.scrutinees.iter().map(Child::Expr).collect();

            for a in &m.arms {
                if let Some(g) = &a.guard {
                    v.push(Child::Expr(g));
                }

                v.push(Child::Block(&a.block));
            }

            if let Some(d) = &m.default {
                v.push(Child::Block(d));
            }

            v
        }

        Stmt::PatternLocal(p) => {
            let mut v = vec![Child::Expr(&p.value)];

            if let Some(b) = &p.else_block {
                v.push(Child::Block(b));
            }

            v
        }

        Stmt::Struct(st) => st
            .fields
            .iter()
            .filter_map(|f| f.default.as_ref().map(Child::Expr))
            .collect(),

        Stmt::Trait(t) => t
            .methods
            .iter()
            .filter_map(|m| m.body.as_ref().map(Child::Function))
            .collect(),

        Stmt::Interface(_) | Stmt::Attribute(_) => Vec::new(),

        Stmt::Remote(r) => r
            .params
            .iter()
            .filter_map(|p| p.default.as_ref().map(Child::Expr))
            .collect(),

        Stmt::Macro(m) => {
            let mut v = vec![Child::Block(&m.body)];

            if let Some(t) = &m.tail {
                v.push(Child::Expr(t));
            }

            v
        }

        Stmt::NumericFor(f) => {
            let mut v = vec![Child::Expr(&f.start), Child::Expr(&f.limit)];

            if let Some(s) = &f.step {
                v.push(Child::Expr(s));
            }

            v.push(Child::Block(&f.block));

            v
        }

        Stmt::GenericFor(f) => {
            let mut v: Vec<Child<'_>> = f.exprs.iter().map(Child::Expr).collect();

            if let Some(c) = &f.filter {
                v.push(Child::Expr(c));
            }

            v.push(Child::Block(&f.block));

            v
        }

        Stmt::Function(f) => function_children(&f.body),

        Stmt::LocalFunction(f) => function_children(&f.body),

        Stmt::Return(r) => r.values.iter().map(Child::Expr).collect(),

        Stmt::Class(c) => c
            .members
            .iter()
            .flat_map(|m| match m {
                ClassMember::Method(f) => function_children(&f.body),

                ClassMember::Field { .. } => Vec::new(),
            })
            .collect(),
    }
}

/// Reports if a function header itself needs rewriting.
fn function_needs_rewrite(body: &FunctionBody) -> bool {
    body.is_async.is_some()
        || body.ret_arrow.is_some()
        || body.has_bounds
        || body
            .params
            .iter()
            .any(|p| p.default.is_some() || p.destructure.is_some())
}

fn for_needs_rewrite(f: &GenericFor) -> bool {
    f.filter.is_some() || f.vars.iter().any(|v| v.destructure.is_some())
}

fn local_needs_rewrite(l: &Local) -> bool {
    l.names.iter().any(|b| b.destructure.is_some())
        || (l.values.len() == 1
            && l.names.len() == 1
            && matches!(
                &l.values[0],
                Expr::New {
                    init: Some(_),
                    args: Some(_),
                    ..
                }
            ))
}

/// Reports if a statement or anything under it needs a rewrite. The walk
/// copies a statement whole when nothing does, which is the common path.
fn stmt_needs_desugar(s: &Stmt) -> bool {
    match s {
        Stmt::Assign(a) if a.op.end - a.op.start == 3 => return true,

        Stmt::Local(l) if local_needs_rewrite(l) => return true,

        Stmt::Delete { .. } => return true,

        Stmt::Function(f) if function_needs_rewrite(&f.body) => return true,

        Stmt::LocalFunction(f) if function_needs_rewrite(&f.body) => return true,

        Stmt::GenericFor(f) if for_needs_rewrite(f) => return true,

        // `global type X = T` is `export type X = T` to Luau; the
        // project index is what the modifier adds.
        Stmt::TypeAlias(t) if t.global => return true,

        Stmt::Import(_)
        | Stmt::ExportList(_)
        | Stmt::ExportDefault { .. }
        | Stmt::Enum(_)
        | Stmt::Impl(_)
        | Stmt::Match(_)
        | Stmt::PatternLocal(_)
        | Stmt::Struct(_)
        | Stmt::Trait(_)
        | Stmt::Interface(_)
        | Stmt::Remote(_)
        | Stmt::Attribute(_)
        | Stmt::Macro(_) => return true,

        Stmt::Function(f) if !f.attrs.is_empty() => return true,

        Stmt::LocalFunction(f) if !f.attrs.is_empty() => return true,

        Stmt::Local(l) if l.exported => return true,

        Stmt::Function(f) if f.exported => return true,

        Stmt::LocalFunction(f) if f.exported => return true,

        Stmt::If(i) if if_has_local(i) => return true,

        Stmt::While(w) if matches!(w.cond, Cond::Local { .. }) => return true,

        _ => {}
    }

    stmt_children(s).iter().any(|c| match c {
        Child::Expr(e) => expr_needs_desugar(e),

        Child::Block(b) => block_needs_desugar(b),

        Child::Function(f) => block_needs_desugar(&f.block),
    })
}

/// Whether a block exports a type, an interface, or another
/// declaration that binds no value at run time. Such a module returns
/// an empty table: Luau requires a module to return exactly one value,
/// and `export type` alone leaves nothing to return.
fn exports_a_type(block: &Block) -> bool {
    block.stmts.iter().any(|s| match s {
        Stmt::TypeAlias(t) => t.exported,

        Stmt::Interface(i) => i.exported,

        Stmt::Trait(t) => t.exported,

        Stmt::Attribute(a) => a.exported,

        Stmt::Macro(m) => m.exported,

        _ => false,
    })
}

/// Whether a statement leaves its block: `return`, `break`, `continue`.
fn is_early_exit(s: &Stmt) -> bool {
    matches!(s, Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_))
}

/// Whether any of these statements carries code; a `;` does not.
fn has_live_stmt(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| !matches!(s, Stmt::Empty(_)))
}

/// Whether a block holds a `return`, a `break`, or a `continue` that is
/// not its last statement. Luau rejects that, so the emit fences it in
/// `do ... end` and the block goes through the renderer.
fn block_has_dead_code(b: &Block) -> bool {
    b.stmts
        .iter()
        .enumerate()
        .any(|(i, s)| is_early_exit(s) && has_live_stmt(&b.stmts[i + 1..]))
}

fn block_needs_desugar(b: &Block) -> bool {
    block_has_dead_code(b) || b.stmts.iter().any(stmt_needs_desugar)
}

fn expr_needs_desugar(e: &Expr) -> bool {
    match e {
        Expr::Binary { op, .. } if op.end - op.start == 2 => return true,

        Expr::Index { optional: true, .. }
        | Expr::Call { optional: true, .. }
        | Expr::Child { .. }
        | Expr::NonNil { .. }
        | Expr::Ternary { .. }
        | Expr::Is { .. }
        | Expr::Satisfies { .. }
        | Expr::Array { .. }
        | Expr::MethodRef { .. }
        | Expr::New { .. }
        | Expr::Await { .. }
        | Expr::Try { .. }
        | Expr::AsyncBlock { .. }
        | Expr::TryBlock { .. }
        | Expr::Macro { .. }
        | Expr::Match(_) => return true,

        Expr::IfElse { branches, .. }
            if branches
                .iter()
                .any(|(c, _)| matches!(c, Cond::Local { .. })) =>
        {
            return true;
        }

        Expr::Function { body, .. } if function_needs_rewrite(body) => return true,

        Expr::Table { fields, .. } if fields.iter().any(|f| matches!(f, TableField::Spread(_))) => {
            return true;
        }

        Expr::Index { .. } | Expr::Call { .. } if chain_has_alloy(e) => return true,

        _ => {}
    }

    expr_children(e).iter().any(|c| match c {
        Child::Expr(e) => expr_needs_desugar(e),

        Child::Block(b) => b.stmts.iter().any(stmt_needs_desugar),

        Child::Function(f) => f.block.stmts.iter().any(stmt_needs_desugar),
    })
}

/// Backticked names joined with commas and a final `and`.
pub(crate) fn list_names(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();

    match quoted.len() {
        0 => String::new(),
        1 => quoted[0].clone(),
        n => format!("{} and {}", quoted[..n - 1].join(", "), quoted[n - 1]),
    }
}

impl<'s> Desugar<'s> {
    fn byte_start(&self, span: TokSpan) -> u32 {
        self.toks[span.start as usize].start
    }

    fn byte_end(&self, span: TokSpan) -> u32 {
        self.toks[span.end as usize - 1].end
    }

    fn text_of(&self, span: TokSpan) -> &'s str {
        &self.src[self.byte_start(span) as usize..self.byte_end(span) as usize]
    }

    fn line_of(&self, byte: u32) -> usize {
        self.src[..byte as usize].matches('\n').count() + 1
    }

    fn where_at(&self, byte: u32) -> String {
        format!("{}:{}", self.options.file_name, self.line_of(byte))
    }

    fn generate(&mut self, anchor: u32, text: &str) {
        if let Err(NewlineInGenerated { anchor, text }) = self.r.generate(anchor, text)
            && !self.generate_spanning(anchor, &text)
        {
            self.diagnostics.push(Diagnostic {
                start: anchor,
                end: anchor,
                message: format!("internal: generated text holds a newline: {text:?}"),
            });
        }
    }

    /// Text assembled from source that spans lines, such as a hoisted
    /// chain prefix with a function literal in it: each piece between the
    /// newlines is generated, and each newline is copied from the source
    /// after the anchor, in order, so the line count and the map hold.
    /// False when the source has too few newlines there.
    fn generate_spanning(&mut self, anchor: u32, text: &str) -> bool {
        let pieces: Vec<&str> = text.split('\n').collect();
        let mut cursor = anchor as usize;
        let mut newlines = Vec::with_capacity(pieces.len() - 1);

        for _ in 1..pieces.len() {
            let Some(at) = self.src[cursor..].find('\n') else {
                return false;
            };
            newlines.push((cursor + at) as u32);
            cursor += at + 1;
        }

        for (i, piece) in pieces.iter().enumerate() {
            let _ = self.r.generate(anchor, piece);

            if let Some(nl) = newlines.get(i) {
                self.r.copy(*nl, nl + 1);
            }
        }

        true
    }

    fn diagnose(&mut self, span: TokSpan, message: &str) {
        self.diagnostics.push(Diagnostic {
            start: self.byte_start(span),
            end: self.byte_end(span),
            message: message.to_string(),
        });
    }

    /*
    Copies a byte range, applying the type edits inside it. `T[]` becomes
    `__alloy.Array<T>` and `read T[]` becomes `{ read T }`. Edits nest, so
    the operand of one copies through this routine again.
    */
    fn copy(&mut self, start: u32, end: u32) {
        if start >= end {
            return;
        }

        // An insert inside the range splits the copy around it.
        if let Some(i) = self
            .inserts
            .iter()
            .position(|(p, _)| start < *p && *p <= end)
        {
            let (at, text) = self.inserts.remove(i);
            self.copy(start, at);
            self.generate(at, &text);
            self.copy(at, end);

            return;
        }

        let edit = self.type_edits.iter().copied().find(|e| match e {
            TypeEdit::ArraySuffix {
                modifier,
                operand,
                brackets,
            } => {
                let s = match modifier {
                    Some(m) => self.byte_start(*m),

                    None => self.byte_start(*operand),
                };

                s >= start && self.byte_end(*brackets) <= end
            }

            TypeEdit::AmbientName(span) => {
                self.byte_start(*span) >= start && self.byte_end(*span) <= end
            }

            TypeEdit::Mapped { table, .. } => {
                self.byte_start(*table) >= start && self.byte_end(*table) <= end
            }
        });

        let edit = match edit {
            Some(TypeEdit::AmbientName(span)) => {
                let (ns, ne) = (self.byte_start(span), self.byte_end(span));
                let name = self.text_of(span).to_string();
                self.r.copy(start, ns);

                if self.is_local(&name) || self.declared_types.contains(&name) {
                    self.r.copy(ns, ne);
                } else if let Some((table, after)) = self.mapped_over_declared(span, end) {
                    self.generate(ns, &table);
                    self.copy(after, end);

                    return;
                } else {
                    let std = self.type_std();
                    self.generate(ns, &format!("{std}{name}"));
                }

                self.copy(ne, end);

                return;
            }

            Some(TypeEdit::Mapped {
                table,
                key,
                source,
                modifier,
                optional,
            }) => {
                let (ts, te) = (self.byte_start(table), self.byte_end(table));
                self.r.copy(start, ts);
                let text = self.mapped_type(key, source, modifier, optional);
                self.generate(ts, &text);
                self.copy(te, end);

                return;
            }

            other => other,
        };

        let Some(TypeEdit::ArraySuffix {
            modifier,
            operand,
            brackets,
        }) = edit
        else {
            self.r.copy(start, end);

            return;
        };

        let edit_start = match modifier {
            Some(m) => self.byte_start(m),

            None => self.byte_start(operand),
        };
        let (op_s, op_e) = (self.byte_start(operand), self.byte_end(operand));
        let br_e = self.byte_end(brackets);

        self.r.copy(start, edit_start);

        match modifier {
            Some(m) => {
                // `read T[]` keeps the non-mutating Array methods, which
                // `{ read T }` would lose; `write T[]` is push and index
                // assignment only, since Luau has no write-only array.
                let word = self.text_of(m).to_string();
                let std = self.std();
                let name = if word == "write" {
                    "WriteArray"
                } else {
                    "ReadArray"
                };
                self.generate(edit_start, &format!("{std}.{name}<"));
                self.copy(op_s, op_e);
                self.generate(op_e, ">");
            }

            None => {
                let std = self.type_std();
                self.generate(edit_start, &format!("{std}Array<"));
                self.copy(op_s, op_e);
                self.generate(op_e, ">");
            }
        }

        self.copy(br_e, end);
    }

    /// Copies a span through the renderer with no rewrite.
    fn copy_span(&mut self, span: TokSpan) {
        if !span.is_empty() {
            let (s, e) = (self.byte_start(span), self.byte_end(span));
            self.copy(s, e);
        }
    }

    // --- scopes for ambient names -------------------------------------------

    fn declare_name(&mut self, span: TokSpan) {
        let name = self.text_of(span).to_string();

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name);
        }
    }

    fn declare_destructure(&mut self, d: &Destructure) {
        match d {
            Destructure::Table(fields) => {
                for f in fields {
                    self.declare_name(f.rename.unwrap_or(f.field));
                }
            }

            Destructure::Array { items, rest } => {
                for i in items {
                    self.declare_name(*i);
                }

                if let Some(r) = rest {
                    self.declare_name(*r);
                }
            }
        }
    }

    fn declare_binding(&mut self, b: &Binding) {
        match &b.destructure {
            None => self.declare_name(b.name),

            Some(d) => self.declare_destructure(d),
        }
    }

    fn declare_params(&mut self, body: &FunctionBody) {
        for p in &body.params {
            match &p.destructure {
                None => self.declare_name(p.name),

                Some(d) => self.declare_destructure(d),
            }
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    /// Reports what a `global` of this file needs: a project to reach,
    /// a module to live in, and a name nothing else owns.
    fn check_globals(&mut self, src: &str, toks: &[Tok], chunk: &Chunk) {
        let decls = crate::globals::declared_in(src, toks, chunk, std::path::Path::new(""));

        // `--@alloy-side` names the side of the global under it, so
        // anywhere else it says nothing. `--@alloy-file-side` is the
        // directive for the whole file.
        let scanned = crate::directives::scan(src);

        let lines: Vec<&str> = src.lines().collect();

        for (line, _) in &scanned.decl_sides {
            // The declaration under the directive, past blank lines,
            // comments, and attributes.
            let mut at = line + 1;

            while lines.get(at).is_some_and(|l| {
                let t = l.trim();

                t.is_empty() || t.starts_with("--") || t.starts_with('@')
            }) {
                at += 1;
            }

            let is_global = decls
                .iter()
                .any(|g| src[..g.start as usize].matches('\n').count() == at);

            if !is_global {
                let (start, end) = crate::directives::span_of_line(src, *line);
                self.diagnostics.push(Diagnostic {
                    start: start as u32,
                    end: end as u32,
                    message: SIDE_ON_GLOBAL.to_string(),
                });
            }
        }

        if decls.is_empty() {
            return;
        }

        let file = self.options.file_name.clone();

        for g in &decls {
            let mut say = |message: String| {
                self.diagnostics.push(Diagnostic {
                    start: g.start,
                    end: g.end,
                    message,
                });
            };

            if !self.options.in_project {
                say("`global` needs a project; there is no everywhere to reach".to_string());

                continue;
            }

            if self.options.definitions {
                say("a declaration file declares; `global` belongs in a module".to_string());

                continue;
            }

            if crate::globals::LUAU_GLOBALS.contains(&g.name.as_str()) {
                say(format!(
                    "`{}` is a Luau global; a project global cannot take its name",
                    g.name
                ));

                continue;
            }

            if let Some((_, ambient)) = self
                .options
                .ambient_clashes
                .iter()
                .find(|(n, _)| n == &g.name)
            {
                say(format!(
                    "`{}` is declared in {ambient} and global in {file}",
                    g.name
                ));

                continue;
            }

            // A script's globals live in the module the build hoists
            // them into, so that module is this file, not another.
            let own = crate::globals::hoist_name(&file);

            if let Some(other) = self
                .options
                .globals
                .iter()
                .find(|o| o.name == g.name && !(self.options.hoist_globals && o.file == own))
            {
                say(format!(
                    "`{}` is global in both {file} and {}",
                    g.name, other.file
                ));

                continue;
            }

            if AMBIENT.contains(&g.name.as_str()) || AMBIENT_TYPES.contains(&g.name.as_str()) {
                self.lints.push(crate::lint::Lint {
                    name: "shadowed_global",
                    start: g.start,
                    end: g.end,
                    message: format!(
                        "`{}` is a std name; the global shadows it in every file",
                        g.name
                    ),
                    fix: None,
                });
            }
        }
    }

    /// Records a use of a project global. The name reaches the file
    /// without an import, so the emit puts the require and the binding
    /// on the first line. A name the file binds itself is the file's own.
    pub(crate) fn use_global(&mut self, name: &str, at: u32) {
        if self.own_names.contains(name)
            || self.is_local(name)
            || self.globals_used.iter().any(|(n, _)| n == name)
        {
            return;
        }

        let Some(g) = self.options.globals.iter().find(|g| g.name == name) else {
            return;
        };

        // A global of one side reaches that side alone. A shared file
        // runs on either side, so it cannot hold one.
        if let Some(theirs) = g.side
            && self.file_side != Some(theirs)
        {
            let word = theirs.name();
            let message = match self.file_side {
                Some(_) => format!("`{name}` is global on the {word} only"),

                None => format!("`{name}` is global on the {word}; this file is shared"),
            };
            self.diagnostics.push(Diagnostic {
                start: at,
                end: at + name.len() as u32,
                message,
            });

            return;
        }

        self.globals_used.push((name.to_string(), at));
    }

    /// The require and the bindings for every global the file named, as
    /// one line. Names from one module share one require.
    fn global_prologue(&self) -> String {
        if self.globals_used.is_empty() || self.options.definitions {
            return String::new();
        }

        // The module order follows the first use, so two builds of one
        // file write the same line.
        let mut modules: Vec<&str> = Vec::new();
        let mut used: Vec<&GlobalRef> = Vec::new();

        for (name, _) in &self.globals_used {
            let Some(g) = self.options.globals.iter().find(|g| &g.name == name) else {
                continue;
            };

            let spec = g.require.as_str();

            if !modules.contains(&spec) {
                modules.push(spec);
            }

            used.push(g);
        }

        let mut out = String::new();

        for (i, spec) in modules.iter().enumerate() {
            let temp = format!("_g{}", i + 1);
            out.push_str(&format!("local {temp} = require({}) ", luau_string(spec)));
            let here: Vec<&&GlobalRef> = used.iter().filter(|g| g.require == *spec).collect();
            let names: Vec<String> = here
                .iter()
                .filter(|g| g.value)
                .map(|g| g.name.clone())
                .collect();

            if !names.is_empty() {
                let values: Vec<String> = names.iter().map(|n| format!("{temp}.{n}")).collect();
                out.push_str(&format!(
                    "local {} = {} ",
                    names.join(", "),
                    values.join(", ")
                ));
            }

            for g in here.iter().filter(|g| g.ty) {
                let args = crate::modules::type_params(&g.type_params);
                out.push_str(&format!(
                    "type {}{} = {temp}.{}{} ",
                    g.name, g.type_params, g.name, args
                ));
            }
        }

        out
    }

    // --- blocks and statements --------------------------------------------

    fn temp_declared(&self, index: u32) -> bool {
        self.declared[self.barrier..]
            .iter()
            .any(|scope| scope.contains(&index))
    }

    fn declare_temp(&mut self, index: u32) {
        if let Some(scope) = self.declared.last_mut() {
            scope.push(index);
        }
    }

    /// Renders an expression into a string, with no effect on the output.
    fn render_to_string(&mut self, e: &Expr) -> String {
        self.render_to_side(e).finish().0
    }

    /// Renders an expression into its own renderer, chunks and all.
    fn render_to_side(&mut self, e: &Expr) -> Renderer<'s> {
        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        self.expr(e);
        std::mem::swap(&mut self.r, &mut side);

        side
    }

    /// Hoists rendered text into a temp and returns the temp's name.
    fn hoist_text(&mut self, value: String, anchor: u32) -> String {
        if self.no_hoist > 0 {
            // The rewrites for `while`, `repeat`, and `elseif` conditions
            // are designed and not built yet. Reading twice here would be
            // wrong silently, so it is a diagnostic and a parenthesized
            // re-read instead.
            self.diagnostics.push(Diagnostic {
                start: anchor,
                end: anchor,
                message: "an operand with side effects is not supported yet inside a `while`, \
                          `repeat`, or `elseif` condition; bind it to a local first"
                    .to_string(),
            });

            return format!("({value})");
        }

        self.temp_next += 1;
        let index = self.temp_next;
        self.hoists.push(Hoist::Temp {
            index,
            value: HoistValue::Text(value),
            anchor,
        });

        format!("_{index}")
    }

    /// Hoists a module require into a local of its own, `_m1`, so its
    /// type is the module's and a type alias through it resolves.
    fn hoist_import(&mut self, path: &str, anchor: u32) -> String {
        self.import_next += 1;
        let name = format!("_m{}", self.import_next);
        self.hoists.push(Hoist::Fresh {
            name: name.clone(),
            value: format!("require({path})"),
            anchor,
        });

        name
    }

    /// Hoists a whole statement in front of the current one.
    fn hoist_stmt(&mut self, text: String, anchor: u32) {
        if self.no_hoist > 0 {
            self.diagnostics.push(Diagnostic {
                start: anchor,
                end: anchor,
                message: "`try` is not supported yet inside a `while`, `repeat`, or `elseif` \
                          condition; bind it to a local first"
                    .to_string(),
            });

            return;
        }

        self.hoists.push(Hoist::Stmt { text, anchor });
    }

    /// Hoists an expression into a temp and returns the temp's name. The
    /// expression renders with its provenance, so one that spans lines,
    /// a function literal as an argument, keeps every line in place.
    fn hoist(&mut self, e: &Expr) -> String {
        let anchor = self.byte_start(e.span());

        if self.no_hoist > 0 {
            let value = self.render_to_string(e);

            return self.hoist_text(value, anchor);
        }

        let rendered = self.render_to_side(e);
        self.temp_next += 1;
        let index = self.temp_next;
        self.hoists.push(Hoist::Temp {
            index,
            value: HoistValue::Rendered(rendered),
            anchor,
        });

        format!("_{index}")
    }

    /// An expression that is cheap and side-effect free to read twice.
    fn is_simple(&self, e: &Expr) -> bool {
        match e {
            Expr::Name(_)
            | Expr::Number(_)
            | Expr::String(_)
            | Expr::Nil(_)
            | Expr::True(_)
            | Expr::False(_) => true,

            Expr::Index {
                object,
                key: IndexKey::Field(_),
                optional: false,
                ..
            } => self.is_simple(object),

            Expr::Paren { inner, .. } => self.is_simple(inner),

            _ => false,
        }
    }

    /// A temp or a simple expression: something safe to name twice.
    fn reusable(&mut self, e: &Expr) -> String {
        if self.is_simple(e) {
            self.render_to_string(e)
        } else {
            self.hoist(e)
        }
    }

    // --- expressions -------------------------------------------------------

    /// The raw constructor the fields form calls: the class table itself
    /// through `__call`, or in the check artifact its typed `__new`.
    fn raw_ctor(&self, name: &str) -> String {
        if self.options.check {
            format!("{name}.__new")
        } else {
            name.to_string()
        }
    }

    /// `(x :: any)` in the check artifact, `x` in the ship artifact: for
    /// a spot where the checker cannot follow what the emit knows.
    fn any_cast(&self, x: &str) -> String {
        if self.options.check {
            format!("({x} :: any)")
        } else {
            x.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::EmitOptions;

    #[test]
    fn a_method_insert_maps_back_to_the_method_name() {
        let src = "type Alias = { z: number }\nimpl Alias as\n    function area(self): number\n        return self.z\n    end\nend\nprint(Alias)\n";
        let options = EmitOptions {
            check: true,
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).unwrap();
        let at = out.check.find("Alias.area").unwrap() as u32;
        let want = src.find("area(self)").unwrap() as u32;

        // Every offset of the inserted `Alias.area` maps to the name the
        // source wrote, never to the space after `function`.
        for i in 0..u32::try_from("Alias.area".len()).unwrap() {
            assert_eq!(out.map.to_source(at + i), want, "offset {i}");
        }
    }
}
