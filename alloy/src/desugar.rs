//! The desugar pass: walks the tree and renders Luau.
//!
//! One walk serves every feature. A node the walk knows how to rewrite
//! renders generated text; every other node copies its source and recurses
//! into its children, so the text between children survives untouched.
//! Statements own the hoists their expressions need, and a block owns the
//! temp names its statements declare, so a temp is declared once per block
//! and assigned on later use. That keeps a long block under Luau's limit
//! of two hundred locals.

use std::collections::HashSet;

use std::collections::HashMap;

use crate::lint::Lint;
use alloy_syntax::ast::{
    Assign, Attr, AttributeDecl, Binding, Block, CallArgs, ChildName, Chunk, ClassMember, Cond,
    Destructure, EnumDecl, ExportList, Expr, Field, FieldPattern, FunctionBody, GenericFor, If,
    ImplDecl, Import, ImportKind, IndexKey, InterfaceDecl, Local, MatchExpr, MatchStmt, Pattern,
    PatternLocal, RemoteDecl, Stmt, StructDecl, TableField, TokSpan, TraitDecl, TraitMethod,
    TypeEdit, While,
};
use alloy_syntax::lexer::{Tok, TokKind};

use crate::render::{NewlineInGenerated, Renderer, SpanMap};
use crate::roblox_classes::{DATATYPES, INSTANCE_CLASSES};

/// Datatypes the definitions declare as a type alias, not a class. A
/// `typeof` test on one names it at run time, but the checker cannot
/// refine a value by it, so the check artifact narrows by a cast.
const ALIAS_DATATYPES: &[&str] = &["RBXScriptSignal"];

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
    /// Per imported trait, the names of its default methods, so an
    /// `impl Trait for S` here flattens them in as a local trait's would.
    pub import_trait_defaults: Vec<(String, Vec<String>)>,
    /// The imported async functions declared to return a `Result`: a
    /// `try await f()` on one is the Result itself. See
    /// `crate::modules::import_result_asyncs`.
    pub import_result_asyncs: Vec<String>,
}

/// `HashMap<string, number>` as `("HashMap", "string, number")`, for the
/// std containers whose constructor takes the arguments. Any other
/// annotation is `None`: a `Signal<T...>` takes a pack, which explicit
/// arguments cannot name, and the annotation alone types it.
fn generic_head(ty: &str) -> Option<(String, String)> {
    // `T[]` is `Array<T>`, so the bracket form names the same head.
    let named = array_types(ty);
    let ty = named.trim();
    let open = ty.find('<')?;
    let base = ty[..open].trim();

    if !matches!(
        base,
        "HashMap" | "Set" | "Array" | "Queue" | "Heap" | "Iter" | "Future"
    ) || !ty.ends_with('>')
    {
        return None;
    }

    let args = &ty[open + 1..ty.len() - 1];

    Some((base.to_string(), args.trim().to_string()))
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
type AttrDecl = (Vec<String>, Vec<(String, Option<String>)>);

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

/// The number widths a parameter or a field may carry.
pub const WIRE_WIDTHS: &[&str] = &["u8", "u16", "u32", "i8", "i16", "i32", "f32", "f64"];

/// One node of a wire layout: how a value packs.
#[derive(Debug, Clone, PartialEq)]
enum Wire {
    /// `u8`, `f64`, `bool`, `str`, or `any` for what crosses as it is.
    Scalar { kind: String, optional: bool },
    /// A record, field by field; `struct` names the local whose
    /// metatable the reader restores.
    Table {
        fields: Vec<(String, Wire)>,
        struct_name: Option<String>,
        optional: bool,
    },
    /// An array: a count, then each item.
    Array { item: Box<Wire>, optional: bool },
}

impl Wire {
    fn is_any(&self) -> bool {
        matches!(self, Wire::Scalar { kind, .. } if kind == "any")
    }

    fn with_optional(mut self, flag: bool) -> Self {
        match &mut self {
            Wire::Scalar { optional, .. }
            | Wire::Table { optional, .. }
            | Wire::Array { optional, .. } => *optional = *optional || flag,
        }

        self
    }

    /// The layout as the Luau table the runtime reads.
    fn luau(&self) -> String {
        match self {
            Wire::Scalar { kind, optional } => {
                luau_string(&format!("{kind}{}", if *optional { "?" } else { "" }))
            }

            Wire::Table {
                fields,
                struct_name,
                optional,
            } => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|(n, w)| format!("{{ {}, {} }}", luau_string(n), w.luau()))
                    .collect();
                let mut out = format!("{{ fields = {{ {} }}", fields.join(", "));

                if let Some(name) = struct_name {
                    out.push_str(&format!(", struct = {name}"));
                }

                if *optional {
                    out.push_str(", optional = true");
                }

                out.push_str(" }");

                out
            }

            Wire::Array { item, optional } => {
                let mut out = format!("{{ item = {}, array = true", item.luau());

                if *optional {
                    out.push_str(", optional = true");
                }

                out.push_str(" }");

                out
            }
        }
    }
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
            import_trait_defaults: Vec::new(),
            import_result_asyncs: Vec::new(),
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
}

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

pub const PRIMITIVES: &[&str] = &[
    "boolean", "number", "string", "table", "function", "thread", "buffer", "vector", "userdata",
];

const WORD_OPS: &[&str] = &["band", "bor", "bxor", "shl", "shr", "in"];

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
        structs_with_new: HashMap::new(),
        impl_target: None,
        declared_types: HashSet::new(),
        traits: HashMap::new(),
        trait_required: HashMap::new(),
        struct_fields: HashMap::new(),
        generic_types: HashSet::new(),
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
    /// Declared trait names with the methods an impl must write: name and
    /// parameter count, `self` included.
    trait_required: HashMap<String, Vec<(String, usize)>>,
    /// Declared struct fields by struct name: field name and whether it
    /// carries a default.
    struct_fields: HashMap<String, Vec<(String, bool)>>,
    /// Structs declared with type parameters: their alias needs
    /// arguments, so the check artifact leaves `self` untyped there.
    generic_types: HashSet<String>,
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
    /// helper table, `__alloy_string.trim(s)`, instead.
    ext_primitive: HashMap<String, String>,
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

/// A macro body captured as one-line source, so an expansion re-parses.
#[derive(Clone)]
struct MacroRef {
    params: Vec<String>,
    variadic: bool,
    /// The statements, tokens joined by spaces.
    body: String,
    /// The trailing expression, tokens joined by spaces.
    tail: Option<String>,
}

enum WordOp {
    Bit,
    In,
}

/// What a pattern compiles to against one access path.
#[derive(Default)]
struct Compiled {
    /// The tests, joined with `and`. Empty means the pattern always matches.
    tests: Vec<String>,
    /// The bindings, as name and access path.
    binds: Vec<(String, String)>,
}

impl<'s> Desugar<'s> {
    // --- modules -----------------------------------------------------------

    /// `import` becomes `require` plus locals or type aliases. A data
    /// path loses its extension: the build writes `data.json` as
    /// `data.luau`, and `require("./data")` finds it.
    /// Whether the module a quoted spec names exports a type by this
    /// name, from the index the caller built.
    fn module_exports_type(&self, quoted: &str, name: &str) -> bool {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options
            .import_types
            .iter()
            .any(|(s, types)| s == spec && types.iter().any(|t| t == name))
    }

    fn import_stmt(&mut self, i: &Import) {
        let anchor = self.byte_start(i.span);
        let path = crate::data::strip_literal(self.text_of(i.path));

        match &i.kind {
            ImportKind::Namespace(n) => {
                let name = self.text_of(*n).to_string();
                self.generate(anchor, &format!("local {name} = require({path})"));
            }

            // `import M, { a } from "p"`: the module binds by its name, and
            // the names read from it: `local M = require("p") local a = M.a`.
            ImportKind::Both(module, specs) => {
                let base = self.text_of(*module).to_string();
                let mut text = format!("local {base} = require({path})");
                let mut names = Vec::new();
                let mut values = Vec::new();
                let mut types = Vec::new();

                for sp in specs {
                    let name = self.text_of(sp.name).to_string();
                    let local = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    if sp.is_type {
                        types.push(format!("type {local} = {base}.{name}"));
                    } else {
                        // A struct or an enum is a value and a type; the
                        // type comes along when the module exports one.
                        if self.module_exports_type(&path, &name) {
                            types.push(format!("type {local} = {base}.{name}"));
                        }

                        names.push(local);
                        values.push(format!("{base}.{name}"));
                    }
                }

                if !names.is_empty() {
                    text.push_str(&format!(
                        " local {} = {}",
                        names.join(", "),
                        values.join(", ")
                    ));
                }

                for t in types {
                    text.push(' ');
                    text.push_str(&t);
                }

                self.generate(anchor, &text);
            }

            ImportKind::Named(specs) => {
                let temp = self.hoist_import(&path, anchor);
                let mut names = Vec::new();
                let mut values = Vec::new();
                let mut types = Vec::new();

                for sp in specs {
                    let name = self.text_of(sp.name).to_string();
                    let local = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    if sp.is_type {
                        types.push(format!("type {local} = {temp}.{name}"));
                    } else {
                        if self.module_exports_type(&path, &name) {
                            types.push(format!("type {local} = {temp}.{name}"));
                        }

                        names.push(local);
                        values.push(format!("{temp}.{name}"));
                    }
                }

                let mut text = String::new();

                if !names.is_empty() {
                    text.push_str(&format!(
                        "local {} = {}",
                        names.join(", "),
                        values.join(", ")
                    ));
                }

                for t in types {
                    if !text.is_empty() {
                        text.push(' ');
                    }

                    text.push_str(&t);
                }

                self.generate(anchor, &text);
            }

            ImportKind::TypeOnly(specs) => {
                // The whole statement exists for the type checker. With
                // `erase_type_imports` it is blanked to ship: every output
                // chunk anchored inside it goes.
                if self.options.erase_type_imports {
                    self.ship_blanks
                        .push((self.byte_start(i.span), self.byte_end(i.span)));
                }
                let temp = self.hoist_import(&path, anchor);
                let parts: Vec<String> = specs
                    .iter()
                    .map(|sp| {
                        let name = self.text_of(sp.name).to_string();
                        let local = sp
                            .alias
                            .map(|a| self.text_of(a).to_string())
                            .unwrap_or(name.clone());

                        format!("type {local} = {temp}.{name}")
                    })
                    .collect();
                self.generate(anchor, &parts.join(" "));
            }
        }
    }

    fn export_list(&mut self, e: &ExportList) {
        let anchor = self.byte_start(e.span);

        match e.from {
            None => {
                for sp in &e.specs {
                    let name = self.text_of(sp.name).to_string();
                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());
                    self.exports.push((exported, name));
                }
            }

            Some(path) => {
                let path = crate::data::strip_literal(self.text_of(path));
                let temp = self.hoist_import(&path, anchor);
                let mut types = Vec::new();

                for sp in &e.specs {
                    let name = self.text_of(sp.name).to_string();
                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    if e.type_only || sp.is_type {
                        types.push(format!("export type {exported} = {temp}.{name}"));
                    } else {
                        self.exports.push((exported, format!("{temp}.{name}")));
                    }
                }

                if !types.is_empty() {
                    self.generate(anchor, &types.join(" "));
                }
            }
        }
    }

    /// `export local x = 1` becomes `local x = 1` and exports `x`.
    fn exported_local(&mut self, span: TokSpan, l: &Local) {
        for b in &l.names {
            let name = self.text_of(b.name).to_string();
            self.exports.push((name.clone(), name));
        }

        // Skip the `export` token; render the rest as a plain local.
        let rest = TokSpan::new(span.start as usize + 1, span.end as usize);

        if local_needs_rewrite(l) {
            self.local_stmt(l);
        } else {
            let children: Vec<Child<'_>> = l.values.iter().map(Child::Expr).collect();
            self.stitch(rest, &children, |d, child| match child {
                Child::Expr(e) => d.expr(e),

                Child::Block(b) => d.block(b),

                Child::Function(b) => d.function_block(b),
            });
        }
    }

    /// The export table, appended after the last token. The test
    /// artifact has no module to return: the spec's footer follows.
    fn module_return(&mut self, at: u32, block: &Block) {
        if self.exports.is_empty() || self.options.tests {
            return;
        }

        if self.has_default_export {
            self.diagnostics.push(Diagnostic {
                start: at,
                end: at,
                message: "a module cannot mix `export default` with named exports".to_string(),
            });

            return;
        }

        if matches!(block.stmts.last(), Some(Stmt::Return(_))) {
            self.diagnostics.push(Diagnostic {
                start: at,
                end: at,
                message: "a module with `export` returns its exports; remove the `return`"
                    .to_string(),
            });

            return;
        }

        let fields: Vec<String> = self
            .exports
            .iter()
            .map(|(k, v)| format!("{k} = {v}"))
            .collect();
        self.generate(at, &format!(" return {{ {} }}", fields.join(", ")));
    }

    // --- enums ---------------------------------------------------------------

    /*
    A payload variant is a constructor that returns a tagged table with the
    enum as its metatable. A unit variant is its own name as a string. The
    type is the union of both, and `is` tests membership. Everything sits
    on the lines the declaration used.
    */
    fn enum_decl(&mut self, e: &EnumDecl) {
        let name = self.text_of(e.name).to_string();
        // The `as` token follows the name, whether or not `export` leads.
        let header_end = self.toks[e.name.end as usize].end;
        let start = self.byte_start(e.span);

        // Header line. An attribute line above `enum` keeps its newline.
        self.generate(
            start,
            &format!("local {name} = {{}} {name}.__index = {name}"),
        );
        self.blank_lines(start, header_end);
        let mut cursor = header_end;
        let mut types = Vec::new();
        let mut unit_tests = Vec::new();

        for v in &e.variants {
            let vs = self.byte_start(v.span);
            let ve = self.byte_end(v.span);
            self.copy_gap_without_commas(cursor, vs);
            let vname = self.text_of(v.name).to_string();

            if let Some(value) = &v.value {
                let val = self.render_to_string(value);
                self.generate(vs, &format!("{name}.{vname} = {val}"));
                types.push(format!("typeof({name}.{vname})"));
                unit_tests.push(format!("v == {name}.{vname}"));
            } else if v.payload.is_empty() {
                // The checker widens a string field to `string`; the cast
                // to the enum keeps `Ok(Zone.Spawn)` a `Result<Zone, _>`.
                let value = if self.options.check {
                    format!("(\"{vname}\" :: {name})")
                } else {
                    format!("\"{vname}\"")
                };
                self.generate(vs, &format!("{name}.{vname} = {value}"));
                types.push(format!("\"{vname}\""));
                unit_tests.push(format!("v == \"{vname}\""));
            } else {
                let params: Vec<String> = (1..=v.payload.len()).map(|i| format!("_{i}")).collect();
                let fields: Vec<String> = (1..=v.payload.len())
                    .map(|i| format!("_{i} = _{i}"))
                    .collect();
                let field_types: Vec<String> = v
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("_{}: {}", i + 1, self.copy_type_to_string(*t)))
                    .collect();
                // The check artifact types the constructor, so its value
                // is the variant's member of the union and `tag` stays a
                // literal, not a string.
                let (plist, ret) = if self.options.check {
                    (field_types.join(", "), format!(": {name}"))
                } else {
                    (params.join(", "), String::new())
                };
                let value = format!(
                    "setmetatable({{ tag = \"{vname}\", {} }}, {name})",
                    fields.join(", ")
                );
                let value = self.any_cast(&value);
                self.generate(
                    vs,
                    &format!("function {name}.{vname}({plist}){ret} return {value} end"),
                );
                // The alias carries the metatable, so a method an `impl`
                // writes on the enum resolves on a payload value.
                types.push(format!(
                    "typeof(setmetatable({{}} :: {{ tag: \"{vname}\", {} }}, {name}))",
                    field_types.join(", ")
                ));
            }

            // An attribute line above the variant keeps its newline.
            self.blank_lines(vs, self.byte_start(v.name));
            cursor = ve;
        }

        let end_tok = self.toks[e.span.end as usize - 1];
        self.copy_gap_without_commas(cursor, end_tok.start);

        let mut test = format!("(type(v) == \"table\" and getmetatable(v) == {name})");

        for t in unit_tests {
            test.push_str(&format!(" or {t}"));
        }

        let export = if e.exported { "export " } else { "" };
        // A variant with a payload prints as `Msg.Move(1, 2)`; a unit
        // variant is a string and prints as its name already.
        let mut printer = if self.options.definitions {
            String::new()
        } else {
            let std = self.std();

            format!(
                " {name}.__tostring = function(v) return {std}.show_variant({}, v) end",
                luau_string(&name)
            )
        };

        // `@derive(Eq)` compares the tag and the payload slots; a unit
        // variant is a string and compares on its own. `Clone` copies a
        // payload variant with its metatable; `Debug` is the printer above.
        let max_arity = e
            .variants
            .iter()
            .map(|v| v.payload.len())
            .max()
            .unwrap_or(0);
        let mut derived: HashSet<String> = HashSet::new();

        for a in &e.attributes {
            let Some(aname) = a.name else { continue };

            if self.text_of(aname) != "derive" {
                continue;
            }

            for arg in &a.args {
                let which = self.text_of(arg.span()).to_string();
                // `Eq` and `PartialEq` write the same `__eq`.
                let key = if which == "PartialEq" { "Eq" } else { &which };

                if !derived.insert(key.to_string()) {
                    continue;
                }

                match which.as_str() {
                    "Eq" | "PartialEq" => {
                        let slots: Vec<String> = (1..=max_arity)
                            .map(|i| format!(" and a._{i} == b._{i}"))
                            .collect();
                        printer.push_str(&format!(
                            " {name}.__eq = function(a: any, b: any): boolean return a.tag == b.tag{} end",
                            slots.join("")
                        ));
                    }

                    "Clone" => {
                        printer.push_str(&format!(
                            " function {name}.clone(v: any): any return if type(v) == \"table\" then setmetatable(table.clone(v), {name}) else v end"
                        ));
                    }

                    _ => {}
                }
            }
        }

        // The attributes on the enum and on its variants, for
        // `Attributes.get(Enum, attr)` and `Attributes.variant`.
        let own = self.attr_table(&e.attributes);
        let variant_attrs: Vec<String> = e
            .variants
            .iter()
            .filter_map(|v| {
                let table = self.attr_table(&v.attributes);

                (table != "{}").then(|| format!("{} = {table}", self.text_of(v.name)))
            })
            .collect();

        if !self.options.definitions && (own != "{}" || !variant_attrs.is_empty()) {
            let std = self.std();
            printer.push_str(&format!(
                " {std}.attrs({name}, {{ own = {own}, variants = {{ {} }} }})",
                variant_attrs.join(", ")
            ));
        }
        self.generate(
            end_tok.start,
            &format!(
                "function {name}.is(v) return {test} end{printer} {export}type {name} = {}",
                types.join(" | ")
            ),
        );

        if e.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// `impl X ... end`: each method lands on `X`; operator traits map to
    /// metamethods.
    fn impl_decl(&mut self, i: &ImplDecl) {
        let target_name = self.text_of(i.target).to_string();
        let start = self.byte_start(i.span);
        // The header runs to the end of the target, and past `<T>` when
        // the impl declares parameters: Luau has no such header.
        let header_end = i
            .generics
            .filter(|g| g.start >= i.target.end)
            .map(|g| self.byte_end(g))
            .unwrap_or_else(|| self.byte_end(i.target));

        if self.options.definitions {
            self.blank_lines(start, self.byte_end(i.span));

            return;
        }

        // A foreign target gets a registry table instead of its metatable.
        let foreign = self.is_foreign(&target_name);
        self.ext_hit |= foreign;
        let target = if foreign {
            "__impl".to_string()
        } else {
            target_name.clone()
        };

        if foreign {
            let std = self.std();
            self.generate(
                start,
                &format!(
                    "do local __impl = {std}.impl_for({})",
                    luau_string(&target_name)
                ),
            );
        } else {
            self.generate(start, "do");
        }
        let mut cursor = header_end;

        // The check artifact types an untyped `self`: a foreign type by
        // its name, a struct or enum of this file by its alias, unless
        // the alias takes parameters that are not in scope here.
        let local_type = (self.structs.contains(&target_name)
            || self.enums.contains_key(&target_name))
            && !self.generic_types.contains(&target_name);

        // `impl Box<T>`: the parameters go on each method, so its body
        // and its signature may name them.
        let impl_generics = i
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let types_self = self.options.check && (foreign || local_type || !impl_generics.is_empty());
        // A struct with private members: a private method lands on
        // `Target__private` in the check artifact, and a public method
        // rebinds `self` to the full view on its first line.
        let split = !foreign && self.has_private_view(&target_name);

        self.impl_target = Some(target_name.clone());

        for m in &i.methods {
            let is_private = m.visibility.is_some_and(|v| self.text_of(v) == "private");

            if types_self {
                self.self_type = Some(if split && is_private {
                    format!("{target_name}__all")
                } else {
                    format!("{target_name}{impl_generics}")
                });
            }

            let has_self = m
                .body
                .params
                .first()
                .is_some_and(|p| self.text_of(p.name) == "self");

            if split && !is_private && has_self {
                self.self_prologue =
                    Some(format!("local self = (self :: any) :: {target_name}__all"));
            }

            let ms = self.byte_start(m.span);
            self.copy(cursor, ms);
            let fn_tok = (m.span.start as usize..m.span.end as usize)
                .find(|&k| self.toks[k].text(self.src) == "function")
                .unwrap_or(m.span.start as usize);
            let fn_tok_end = self.toks[fn_tok].end;
            let name_span = m.path[0];
            let mname = self.text_of(name_span).to_string();
            // `function name(` becomes `function Target.name(`. The
            // `private` or `public` word has no Luau form and goes.
            match m.visibility {
                Some(v) => {
                    self.copy(ms, self.byte_start(v));
                    self.copy(self.byte_end(v), fn_tok_end);
                }

                None => self.copy(ms, fn_tok_end),
            }

            let owner = if split && is_private {
                format!("{target}__private")
            } else {
                target.clone()
            };
            // The insert anchors on the method's own name, not on the
            // gap after `function`: a diagnostic the checker puts on the
            // inserted owner then lands on a name the source shows.
            // A method of a generic impl carries the impl's parameters,
            // unless it declares its own.
            let method_generics = if m.body.generics.is_none() {
                impl_generics.as_str()
            } else {
                ""
            };
            self.generate(
                self.byte_start(name_span),
                &format!(" {owner}.{mname}{method_generics}"),
            );
            let after_name = self.byte_end(name_span);
            let rest = TokSpan::new(name_span.end as usize, m.span.end as usize);
            let _ = after_name;

            if function_needs_rewrite(&m.body) || self.self_type.is_some() {
                self.function_with_header(rest, &m.body);
            } else {
                let children = function_children(&m.body);
                self.stitch(rest, &children, |d, child| match child {
                    Child::Expr(e) => d.expr(e),

                    Child::Block(b) => d.block(b),

                    Child::Function(b) => d.function_block(b),
                });
            }

            cursor = self.byte_end(m.span);
            self.self_prologue = None;
        }

        self.self_type = None;
        self.impl_target = None;

        let end_tok = self.toks[i.span.end as usize - 1];
        self.copy(cursor, end_tok.start);

        // Operator traits.
        let mut tail = String::new();

        if let Some(t) = i.trait_name {
            let trait_name = self.text_of(t).to_string();
            let mapping: &[(&str, &str, &str)] = &[
                ("Add", "add", "__add"),
                ("Sub", "sub", "__sub"),
                ("Mul", "mul", "__mul"),
                ("Div", "div", "__div"),
                ("Eq", "eq", "__eq"),
                ("Lt", "lt", "__lt"),
                ("Le", "le", "__le"),
                ("Display", "to_string", "__tostring"),
                ("Call", "call", "__call"),
                ("Len", "len", "__len"),
                ("Concat", "concat", "__concat"),
                ("Drop", "drop", "Destroy"),
            ];

            for (tr, method, meta) in mapping {
                if trait_name == *tr {
                    // `delete` takes a `Deletable`, whose `Destroy` is
                    // `(self: any) -> ()`; the check artifact says so.
                    let value = if self.options.check && *tr == "Drop" {
                        format!("({target}.{method} :: (self: any) -> ())")
                    } else {
                        format!("{target}.{method}")
                    };
                    tail.push_str(&format!(" {target}.{meta} = {value}"));
                }
            }

            // A trait declared in this file is a contract: every method
            // without a body appears in the impl, with the same arity.
            if let Some(required) = self.trait_required.get(&trait_name).cloned() {
                for (m, arity) in required {
                    let written = i.methods.iter().find(|f| self.text_of(f.path[0]) == m);

                    match written {
                        None => self.diagnose(
                            t,
                            &format!(
                                "`impl {trait_name} for {target_name}` does not write `{m}`; the trait declares it"
                            ),
                        ),

                        Some(f)
                            if f.body.params.len() != arity
                                && !f.body.params.iter().any(|p| p.is_vararg) =>
                        {
                            self.diagnose(
                                f.path[0],
                                &format!(
                                    "the trait method `{m}` takes {} parameter{} in `{trait_name}`, {} here",
                                    arity,
                                    if arity == 1 { "" } else { "s" },
                                    f.body.params.len()
                                ),
                            );
                        }

                        _ => {}
                    }
                }
            }

            // Default methods of a trait declared in this file flatten in.
            // The check artifact assigns without the guard: a conditional
            // assignment would make the property optional to the checker,
            // and the struct would then not satisfy the trait.
            let defaults = self.traits.get(&trait_name).cloned().or_else(|| {
                self.options
                    .import_trait_defaults
                    .iter()
                    .find(|(t, _)| *t == trait_name)
                    .map(|(_, d)| d.clone())
            });

            if let Some(defaults) = defaults {
                let written: Vec<String> = i
                    .methods
                    .iter()
                    .map(|f| self.text_of(f.path[0]).to_string())
                    .collect();

                for m in defaults {
                    if self.options.check {
                        // The impl's own method keeps its type.
                        if !written.contains(&m) {
                            tail.push_str(&format!(" {target}.{m} = {trait_name}.{m}"));
                        }
                    } else {
                        tail.push_str(&format!(
                            " if rawget({target}, \"{m}\") == nil then {target}.{m} = {trait_name}.{m} end"
                        ));
                    }
                }
            }
        }

        // A struct's `end` line carried its tables; the impl adds after.
        if i.exported && !foreign {
            self.exports
                .push((target_name.clone(), target_name.clone()));
        }

        self.generate(end_tok.start, &format!("end{tail}"));
    }

    // --- patterns ------------------------------------------------------------

    fn renamed(&self, name: &str) -> Option<String> {
        self.renames.iter().rev().find_map(|m| m.get(name).cloned())
    }

    /// Reports if a bare name is a unit variant of a known enum.
    fn unit_variant_of(&self, name: &str) -> Option<String> {
        self.enums
            .iter()
            .find(|(_, vs)| vs.iter().any(|(v, n)| v == name && *n == 0))
            .map(|(e, _)| e.clone())
    }

    /// Compiles a pattern against an access path.
    fn compile_pattern(&mut self, p: &Pattern, path: &str, out: &mut Compiled) {
        match p {
            Pattern::Wildcard(_) => {}

            Pattern::Bind(name) => {
                let n = self.text_of(*name).to_string();

                if self.unit_variant_of(&n).is_some() {
                    out.tests.push(format!("{path} == \"{n}\""));
                } else {
                    out.binds.push((n, path.to_string()));
                }
            }

            Pattern::Literal(e) => {
                let lit = self.render_to_string(e);
                out.tests.push(format!("{path} == {lit}"));
            }

            Pattern::Path(span) => {
                let text = self.text_of(*span).to_string();
                out.tests.push(format!("{path} == {text}"));
            }

            Pattern::Variant { name, args, .. } => {
                let vname = self.text_of(*name).to_string();
                out.tests.push(format!(
                    "type({path}) == \"table\" and {path}.tag == \"{vname}\""
                ));

                for (i, a) in args.iter().enumerate() {
                    let sub = format!("{path}._{}", i + 1);
                    self.compile_pattern(a, &sub, out);
                }
            }

            Pattern::Struct { name, fields, .. } => {
                match name {
                    Some(n) => {
                        let sname = self.text_of(*n).to_string();
                        out.tests
                            .push(format!("getmetatable({}) == {sname}", self.any_cast(path)));
                    }

                    None => out.tests.push(format!("type({path}) == \"table\"")),
                }

                for FieldPattern { field, pattern } in fields {
                    let fname = self.text_of(*field).to_string();
                    let sub = format!("{path}.{fname}");

                    match pattern {
                        Some(sub_pat) => self.compile_pattern(sub_pat, &sub, out),

                        None => out.binds.push((fname, sub)),
                    }
                }
            }

            Pattern::Array { items, rest, .. } => {
                let op = if rest.is_some() { ">=" } else { "==" };
                out.tests.push(format!(
                    "type({path}) == \"table\" and #{path} {op} {}",
                    items.len()
                ));

                for (i, item) in items.iter().enumerate() {
                    let sub = format!("{path}[{}]", i + 1);
                    self.compile_pattern(item, &sub, out);
                }

                if let Some(r) = rest {
                    let rname = self.text_of(*r).to_string();
                    let std = self.std();
                    out.binds.push((
                        rname,
                        format!("{std}.Array.slice({path}, {})", items.len() + 1),
                    ));
                }
            }

            Pattern::Or(a, b, span) => {
                let mut ca = Compiled::default();
                let mut cb = Compiled::default();
                self.compile_pattern(a, path, &mut ca);
                self.compile_pattern(b, path, &mut cb);
                let ta = self.cast_tests(&join_tests(&ca.tests), path);
                let tb = self.cast_tests(&join_tests(&cb.tests), path);
                out.tests.push(format!("(({ta}) or ({tb}))"));

                let names_a: Vec<&String> = ca.binds.iter().map(|(n, _)| n).collect();
                let names_b: Vec<&String> = cb.binds.iter().map(|(n, _)| n).collect();

                if names_a != names_b {
                    self.diagnose(
                        *span,
                        "both sides of an `or` pattern must bind the same names",
                    );
                }

                for (n, pa) in &ca.binds {
                    let pb = cb
                        .binds
                        .iter()
                        .find(|(m, _)| m == n)
                        .map(|(_, p)| p.clone())
                        .unwrap_or_else(|| pa.clone());
                    // The checker refines each side by its own tag and
                    // cannot pick one across the `or`; the check artifact
                    // reads the payload untyped.
                    let (pa, pb) = (self.cast_root(pa), self.cast_root(&pb));
                    out.binds
                        .push((n.clone(), format!("(if {ta} then {pa} else {pb})")));
                }
            }
        }
    }

    /// The test text for several patterns against several paths, plus a
    /// guard rendered with the bindings substituted.
    fn arm_test(
        &mut self,
        patterns: &[Pattern],
        paths: &[String],
        guard: Option<&Expr>,
    ) -> (String, Compiled) {
        let mut c = Compiled::default();

        for (p, path) in patterns.iter().zip(paths) {
            self.compile_pattern(p, path, &mut c);
        }

        let mut test = join_tests(&c.tests);

        if let Some(g) = guard {
            let map: HashMap<String, String> = c.binds.iter().cloned().collect();
            self.renames.push(map);
            let text = self.render_to_string(g);
            self.renames.pop();
            test = if test == "true" {
                format!("({text})")
            } else {
                format!("{test} and ({text})")
            };
        }

        (test, c)
    }

    /// Checks the variant patterns of a match against the enum they name.
    ///
    /// A pattern whose variant belongs to a known enum must bind the
    /// payload the variant carries. A name no enum owns is a missing
    /// variant of the enum the other arms name.
    fn check_variant_patterns(&mut self, arms: &[&[Pattern]]) -> bool {
        let mut reported = false;
        let mut flat: Vec<(TokSpan, usize)> = Vec::new();

        for pats in arms {
            let mut stack: Vec<&Pattern> = pats.iter().collect();

            while let Some(p) = stack.pop() {
                match p {
                    Pattern::Or(a, b, _) => {
                        stack.push(a);
                        stack.push(b);
                    }

                    Pattern::Variant { name, args, .. } => {
                        flat.push((*name, args.len()));

                        for a in args {
                            stack.push(a);
                        }
                    }

                    _ => {}
                }
            }
        }

        // The enum of the match: the first variant name a declared enum
        // owns. A name none owns is then a variant that enum lacks.
        let owner = flat.iter().find_map(|(name, _)| {
            let vname = self.text_of(*name);

            self.enums
                .iter()
                .find(|(_, vs)| vs.iter().any(|(v, _)| v == vname))
                .map(|(e, _)| e.clone())
        });

        for (name, binds) in flat {
            let vname = self.text_of(name).to_string();
            let found = self
                .enums
                .iter()
                .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                .map(|(e, vs)| {
                    (
                        e.clone(),
                        vs.iter()
                            .find(|(v, _)| *v == vname)
                            .map(|(_, n)| *n)
                            .unwrap_or(0),
                    )
                });

            match found {
                Some((_, arity)) if arity != binds => {
                    let message = format!(
                        "the variant `{vname}` carries {}, the arm binds {binds}",
                        if arity == 1 {
                            "1 value".to_string()
                        } else {
                            format!("{arity} values")
                        }
                    );
                    self.diagnose(name, &message);
                    reported = true;
                }

                Some(_) => {}

                None => {
                    if let Some(e) = &owner
                        && let Some(vs) = self.enum_decls.get(e)
                    {
                        let names: Vec<&str> = vs.iter().map(|(v, _)| v.as_str()).collect();
                        let message = format!(
                            "`{e}` has no variant `{vname}`; its variants are {}",
                            list_names(&names)
                        );
                        self.diagnose(name, &message);
                        reported = true;
                    }
                }
            }
        }

        reported
    }

    /// Checks single-level exhaustiveness over a known enum.
    fn match_is_exhaustive(&self, arms: &[&[Pattern]], guards: &[bool]) -> bool {
        // One scrutinee; a guarded arm proves nothing.
        let mut column: Vec<&Pattern> = Vec::new();

        for (pats, guarded) in arms.iter().zip(guards) {
            let [p] = pats else {
                return false;
            };

            if !*guarded {
                column.push(p);
            }
        }

        self.column_covers(&column)
    }

    /// The lints on a `default` arm: one that cannot run, and one with
    /// nothing in it. The arm is the `default` token after the arms.
    fn default_lints(&mut self, span: TokSpan, arms_end: u32, exhaustive: bool, empty: bool) {
        let at = (arms_end as usize..span.end as usize)
            .find(|&i| self.text_of(TokSpan::new(i, i + 1)) == "default")
            .map(|i| self.toks[i])
            .unwrap_or(self.toks[span.start as usize]);

        if exhaustive {
            self.lints.push(Lint {
                name: "unreachable_default",
                start: at.start,
                end: at.end,
                message: "this `default` never runs: the arms cover every variant; delete it so a new variant is a missing arm, not a silent fallback".to_string(),
                fix: None,
            });
        }

        if empty {
            self.lints.push(Lint {
                name: "empty_default",
                start: at.start,
                end: at.end,
                message: "this `default` is empty and swallows every variant without an arm; name them, or write the fallback".to_string(),
                fix: None,
            });
        }
    }

    /// The diagnostic for a match with no `default` that leaves values
    /// out. When the arms name variants of one enum, the message lists the
    /// variants with no arm.
    fn not_exhaustive_message(&self, arms: &[&[Pattern]]) -> String {
        let generic = "this match is not exhaustive; add a `default` arm".to_string();
        let mut named: Vec<String> = Vec::new();
        let mut enum_name: Option<String> = None;

        for pats in arms {
            let mut stack: Vec<&Pattern> = pats.iter().collect();

            while let Some(p) = stack.pop() {
                match p {
                    Pattern::Or(a, b, _) => {
                        stack.push(a);
                        stack.push(b);
                    }

                    Pattern::Bind(n) => {
                        let name = self.text_of(*n).to_string();

                        if let Some(e) = self.unit_variant_of(&name) {
                            enum_name.get_or_insert(e);
                            named.push(name);
                        }
                    }

                    Pattern::Path(span) => {
                        let text = self.text_of(*span).to_string();

                        if let Some((e, v)) = text.split_once('.')
                            && self.enums.contains_key(e)
                        {
                            enum_name.get_or_insert(e.to_string());
                            named.push(v.to_string());
                        }
                    }

                    // `case "Red"` matches a unit variant: the emit
                    // compares the same string.
                    Pattern::Literal(v) => {
                        if let Expr::String(span) = v.as_ref() {
                            let text = self.text_of(*span);
                            let name = text.trim_matches(|c| c == '"' || c == '\'').to_string();

                            if let Some(e) = self.unit_variant_of(&name) {
                                enum_name.get_or_insert(e);
                                named.push(name);
                            }
                        }
                    }

                    Pattern::Variant { name, .. } => {
                        let vname = self.text_of(*name).to_string();
                        let owner = self
                            .enums
                            .iter()
                            .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                            .map(|(e, _)| e.clone());

                        if let Some(e) = owner {
                            enum_name.get_or_insert(e);
                            named.push(vname);
                        }
                    }

                    _ => {}
                }
            }
        }

        let Some(e) = enum_name else {
            return generic;
        };
        let missing: Vec<&str> = self.enums[&e]
            .iter()
            .filter(|(v, _)| !named.contains(v))
            .map(|(v, _)| v.as_str())
            .collect();

        if missing.is_empty() {
            return generic;
        }

        format!(
            "this match is not exhaustive: `{e}` has no arm for {}; add {} or a `default` arm",
            list_names(&missing),
            if missing.len() == 1 { "it" } else { "them" }
        )
    }

    /// Reports if a set of patterns over one value leaves no value out.
    ///
    /// An irrefutable pattern covers everything. Enum variants cover their
    /// enum when every variant appears with covering payloads. Array
    /// patterns cover when an open pattern at length k comes with every
    /// exact length below k.
    fn column_covers(&self, column: &[&Pattern]) -> bool {
        // Flatten `a or b` into two rows.
        let mut flat: Vec<&Pattern> = Vec::new();

        for p in column {
            let mut stack: Vec<&Pattern> = vec![p];

            while let Some(q) = stack.pop() {
                match q {
                    Pattern::Or(a, b, _) => {
                        stack.push(a.as_ref());
                        stack.push(b.as_ref());
                    }

                    other => flat.push(other),
                }
            }
        }

        let mut enum_name: Option<String> = None;
        // Variant name -> the payload rows seen for it.
        let mut rows: Vec<(String, Vec<&Pattern>)> = Vec::new();
        let mut exact_lengths: HashSet<usize> = HashSet::new();
        let mut open_from: Option<usize> = None;

        for p in flat {
            match p {
                Pattern::Wildcard(_) => return true,

                Pattern::Bind(n) => {
                    let name = self.text_of(*n).to_string();

                    match self.unit_variant_of(&name) {
                        Some(e) => {
                            enum_name.get_or_insert(e);
                            rows.push((name, Vec::new()));
                        }

                        None => return true,
                    }
                }

                Pattern::Array { items, rest, .. } => {
                    let all_bind = items
                        .iter()
                        .all(|i| matches!(i, Pattern::Wildcard(_) | Pattern::Bind(_)));

                    if !all_bind {
                        return false;
                    }

                    match rest {
                        Some(_) => {
                            open_from = Some(open_from.map_or(items.len(), |o| o.min(items.len())));
                        }

                        None => {
                            exact_lengths.insert(items.len());
                        }
                    }
                }

                Pattern::Path(span) => {
                    // `Color.Red` covers the unit variant `Red` of `Color`.
                    let text = self.text_of(*span).to_string();
                    let Some((e, v)) = text.split_once('.') else {
                        return false;
                    };

                    match self.enums.get(e) {
                        Some(vs) if vs.iter().any(|(n, c)| n == v && *c == 0) => {
                            enum_name.get_or_insert(e.to_string());
                            rows.push((v.to_string(), Vec::new()));
                        }

                        _ => return false,
                    }
                }

                Pattern::Variant { name, args, .. } => {
                    let vname = self.text_of(*name).to_string();
                    let owner = self
                        .enums
                        .iter()
                        .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                        .map(|(e, _)| e.clone());

                    let Some(e) = owner else {
                        return false;
                    };

                    enum_name.get_or_insert(e);
                    rows.push((vname, args.iter().collect()));
                }

                _ => return false,
            }
        }

        if let Some(k) = open_from
            && enum_name.is_none()
            && (0..k).all(|n| exact_lengths.contains(&n))
        {
            return true;
        }

        let Some(e) = enum_name else {
            return false;
        };

        let variants = self.enums[&e].clone();

        variants.iter().all(|(v, arity)| {
            let payloads: Vec<&Vec<&Pattern>> = rows
                .iter()
                .filter(|(n, _)| n == v)
                .map(|(_, args)| args)
                .collect();

            if payloads.is_empty() {
                return false;
            }

            if *arity == 0 {
                return true;
            }

            self.payloads_cover(&payloads, *arity)
        })
    }

    /// Reports if the payload rows of one variant cover every payload.
    ///
    /// A row of irrefutable patterns covers. Otherwise one field must be
    /// the only refutable field in every row, and that field's column must
    /// cover on its own.
    fn payloads_cover(&self, rows: &[&Vec<&Pattern>], arity: usize) -> bool {
        let irrefutable = |p: &Pattern| matches!(p, Pattern::Wildcard(_) | Pattern::Bind(_));

        if rows.iter().any(|r| r.iter().all(|p| irrefutable(p))) {
            return true;
        }

        (0..arity).any(|j| {
            let others_bind = rows
                .iter()
                .all(|r| r.iter().enumerate().all(|(i, p)| i == j || irrefutable(p)));

            if !others_bind {
                return false;
            }

            let column: Vec<&Pattern> = rows.iter().filter_map(|r| r.get(j).copied()).collect();

            self.column_covers(&column)
        })
    }

    /// `match` as a statement: an if-chain on temps, one arm per line.
    fn match_stmt(&mut self, m: &MatchStmt) {
        let start = self.byte_start(m.span);
        let with_end = self.toks[m
            .arms
            .first()
            .map(|a| a.span.start)
            .unwrap_or(m.span.end - 1) as usize
            - 1]
        .end;

        // `match a, b with` becomes `do local _1 = a local _2 = b`.
        let mut paths = Vec::new();
        let mut header = String::from("do");

        for sc in &m.scrutinees {
            let value = self.render_to_string(sc);
            self.temp_next += 1;
            let index = self.temp_next;
            header.push_str(&format!(" local _m{index} = {value}"));
            paths.push(format!("_m{index}"));
        }

        self.generate(start, &header);
        let mut cursor = with_end;

        let pats: Vec<&[Pattern]> = m.arms.iter().map(|a| a.patterns.as_slice()).collect();
        let guards: Vec<bool> = m.arms.iter().map(|a| a.guard.is_some()).collect();

        let exhaustive = self.match_is_exhaustive(&pats, &guards);
        // A rejected pattern makes the arm list unreliable, so the
        // exhaustiveness message would name the wrong variant.
        let bad_arm = self.check_variant_patterns(&pats);

        if m.default.is_none() && !exhaustive && !bad_arm {
            let msg = self.not_exhaustive_message(&pats);
            self.diagnose(m.span, &msg);
        }

        if let Some(d) = &m.default {
            let arms_end = m.arms.last().map_or(m.span.start, |a| a.span.end);
            self.default_lints(m.span, arms_end, exhaustive, d.stmts.is_empty());
        }

        for (i, arm) in m.arms.iter().enumerate() {
            let arm_start = self.byte_start(arm.span);
            self.copy(cursor, arm_start);
            let (test, c) = self.arm_test(&arm.patterns, &paths, arm.guard.as_ref());
            let keyword = if i == 0 { "if" } else { "elseif" };
            let mut text = format!("{keyword} {test} then");

            if !c.binds.is_empty() {
                let names: Vec<String> = c.binds.iter().map(|(n, _)| n.clone()).collect();
                let values: Vec<String> = c.binds.iter().map(|(_, p)| p.clone()).collect();
                text.push_str(&format!(
                    " local {} = {}",
                    names.join(", "),
                    values.join(", ")
                ));
            }

            // The arm head runs to `then`; the block follows.
            let then_tok = self.find_tok_after(
                arm.patterns
                    .last()
                    .map(|p| p.span().end)
                    .unwrap_or(arm.span.start),
                "then",
            );
            let then_end = self.toks[then_tok as usize].end;
            self.generate(arm_start, &text);
            cursor = then_end;

            self.scopes.push(HashSet::new());

            for (n, _) in &c.binds {
                let names = n.clone();

                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(names);
                }
            }

            let body_start = self.block_start_or(&arm.block, cursor);
            self.copy(cursor, body_start);
            self.block(&arm.block);
            self.scopes.pop();
            cursor = self.block_end_or(&arm.block, body_start);
        }

        if let Some(d) = &m.default {
            // The `default` token sits before the block.
            let default_tok = self.toks[d.span.start as usize - 1];
            let default_tok = if d.span.is_empty() {
                self.toks[m.span.end as usize - 2]
            } else {
                default_tok
            };
            self.copy(cursor, default_tok.start);
            self.generate(default_tok.start, "else");
            cursor = default_tok.end;
            let body_start = self.block_start_or(d, cursor);
            self.copy(cursor, body_start);
            self.block(d);
            cursor = self.block_end_or(d, body_start);
        }

        let end_tok = self.toks[m.span.end as usize - 1];
        self.copy(cursor, end_tok.start);

        // Every variant has an arm, so the chain has no `else` and the
        // checker reads a path that falls through. The raise closes it,
        // and an exhaustive match whose arms all return counts as one.
        if m.default.is_none() && exhaustive && !m.arms.is_empty() {
            self.generate(
                end_tok.start,
                "else error(\"match: no arm covers this value\", 2) ",
            );
        }

        self.copy(end_tok.start, end_tok.end);
        self.generate(end_tok.end, " end");
    }

    /// `match` as an expression: nested if-expressions with bindings
    /// substituted, arms staying on their lines.
    fn match_expr(&mut self, m: &MatchExpr) {
        let start = self.byte_start(m.span);
        let mut paths = Vec::new();

        for sc in &m.scrutinees {
            let p = self.reusable(sc);
            paths.push(p);
        }

        let pats: Vec<&[Pattern]> = m.arms.iter().map(|a| a.patterns.as_slice()).collect();
        let guards: Vec<bool> = m.arms.iter().map(|a| a.guard.is_some()).collect();
        let exhaustive = self.match_is_exhaustive(&pats, &guards);
        // A rejected pattern makes the arm list unreliable, so the
        // exhaustiveness message would name the wrong variant.
        let bad_arm = self.check_variant_patterns(&pats);

        if m.default.is_none() && !exhaustive && !bad_arm {
            let msg = self.not_exhaustive_message(&pats);
            self.diagnose(m.span, &msg);
        }

        if let Some(d) = &m.default {
            let arms_end = m.arms.last().map_or(m.span.start, |a| a.span.end);
            let empty = matches!(d.as_ref(), Expr::Nil(_));
            self.default_lints(m.span, arms_end, exhaustive, empty);
        }

        let with_end = self.toks[m
            .arms
            .first()
            .map(|a| a.span.start)
            .unwrap_or(m.span.end - 1) as usize
            - 1]
        .end;
        self.generate(start, "(");
        let mut cursor = with_end;
        let last_index = m.arms.len().saturating_sub(1);

        for (i, arm) in m.arms.iter().enumerate() {
            let arm_start = self.byte_start(arm.span);
            self.copy(cursor, arm_start);
            let (test, c) = self.arm_test(&arm.patterns, &paths, arm.guard.as_ref());
            let is_last_without_default = m.default.is_none() && i == last_index && exhaustive;
            let keyword = if is_last_without_default {
                "else".to_string()
            } else if i == 0 {
                format!("if {test} then")
            } else {
                format!("elseif {test} then")
            };
            self.generate(arm_start, &keyword);
            let then_tok = self.find_tok_after(
                arm.patterns
                    .last()
                    .map(|p| p.span().end)
                    .unwrap_or(arm.span.start),
                "then",
            );
            let then_end = self.toks[then_tok as usize].end;
            cursor = then_end;
            let vs = self.byte_start(arm.value.span());
            self.copy(cursor, vs);
            let map: HashMap<String, String> = c.binds.iter().cloned().collect();
            self.renames.push(map);
            self.expr(&arm.value);
            self.renames.pop();
            cursor = self.byte_end(arm.value.span());
        }

        if let Some(d) = &m.default {
            let default_tok = self.toks[d.span().start as usize - 1];
            self.copy(cursor, default_tok.start);
            self.generate(default_tok.start, "else");
            cursor = default_tok.end;
            let vs = self.byte_start(d.span());
            self.copy(cursor, vs);
            self.expr(d);
            cursor = self.byte_end(d.span());
        } else if !exhaustive {
            self.generate(cursor, " else nil");
        }

        let end_tok = self.toks[m.span.end as usize - 1];
        self.copy(cursor, end_tok.start);
        self.generate(end_tok.start, ")");
    }

    /// `local Ok(v) = e` and let-else.
    fn pattern_local(&mut self, p: &PatternLocal) {
        let anchor = self.byte_start(p.span);
        let value = self.render_to_string(&p.value);
        let temp = self.hoist_text(value, anchor);
        let mut c = Compiled::default();
        self.compile_pattern(&p.pattern, &temp, &mut c);
        let test = join_tests(&c.tests);
        // Luau has `const` of its own, so the keyword passes through and
        // a reassignment is Luau's compile error.
        let keyword = self.text_of(p.keyword).to_string();
        let binds = if c.binds.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = c.binds.iter().map(|(n, _)| n.clone()).collect();
            let values: Vec<String> = c.binds.iter().map(|(_, v)| v.clone()).collect();

            format!("{keyword} {} = {}", names.join(", "), values.join(", "))
        };

        match &p.else_block {
            None => {
                let pat = self.text_of(p.pattern.span()).to_string();
                self.hoist_stmt(
                    format!(
                        "if not ({test}) then error({} .. tostring(if type({temp}) == \"table\" then {temp}.tag else {temp})) end",
                        luau_string(&format!("pattern `{pat}` did not match, got "))
                    ),
                    anchor,
                );
                self.generate(anchor, &binds);
            }

            Some(block) => {
                self.generate(anchor, &format!("if not ({test}) then"));
                let else_tok = self.toks[block.span.start as usize - 1];
                let else_tok = if block.span.is_empty() {
                    self.toks[p.span.end as usize - 2]
                } else {
                    else_tok
                };
                let body_start = self.block_start_or(block, else_tok.end);
                self.copy(else_tok.end, body_start);
                self.block(block);
                let after = self.block_end_or(block, body_start);
                let end_tok = self.toks[p.span.end as usize - 1];
                self.copy(after, end_tok.start);
                self.copy(end_tok.start, end_tok.end);

                if !binds.is_empty() {
                    self.generate(end_tok.end, &format!(" {binds}"));
                }
            }
        }
    }

    // --- conditions with bindings -------------------------------------------

    /// The declarations and test for a `Cond::Local`, for a block context
    /// where temps and bindings can be locals.
    fn cond_local_parts(&mut self, cond: &Cond) -> (Vec<String>, String, Vec<(String, String)>) {
        let Cond::Local {
            negated,
            bindings,
            filter,
            ..
        } = cond
        else {
            unreachable!("only local conditions have parts");
        };

        let mut decls = Vec::new();
        let mut tests = Vec::new();
        let mut binds: Vec<(String, String)> = Vec::new();
        let mut prior: Vec<String> = Vec::new();

        for b in bindings {
            // An earlier name in this condition is its temp by now.
            let map: HashMap<String, String> = binds.iter().cloned().collect();
            self.renames.push(map);
            let value = self.render_to_string(&b.value);
            self.renames.pop();
            let ty =
                b.ty.map(|t| format!(": {}", self.text_of(t)))
                    .unwrap_or_default();
            // A later binding runs only when the earlier ones are truthy.
            let value = if prior.is_empty() {
                value
            } else {
                format!("if {} then {value} else nil", prior.join(" and "))
            };

            match &b.pattern {
                // The branch declares the name from a temp the test refined,
                // so the name is `T`, not `T?`, in the branch and in any
                // closure there. A negated condition keeps the name, since
                // it must stay in scope after a guard clause.
                Pattern::Bind(n) if self.unit_variant_of(self.text_of(*n)).is_none() => {
                    let name = self.text_of(*n).to_string();

                    if *negated {
                        decls.push(format!("local {name}{ty} = {value}"));
                        tests.push(name.clone());
                        prior.push(name);
                    } else {
                        self.temp_next += 1;
                        let temp = format!("_c{}", self.temp_next);
                        decls.push(format!("local {temp}{ty} = {value}"));
                        tests.push(temp.clone());
                        prior.push(temp.clone());
                        binds.push((name, temp));
                    }
                }

                pat => {
                    self.temp_next += 1;
                    let temp = format!("_c{}", self.temp_next);
                    decls.push(format!("local {temp} = {value}"));
                    let mut c = Compiled::default();
                    self.compile_pattern(pat, &temp, &mut c);
                    let test = join_tests(&c.tests);
                    tests.push(format!("({test})"));
                    prior.push(format!("({test})"));
                    binds.extend(c.binds);
                }
            }
        }

        let mut test = tests.join(" and ");

        if let Some(f) = filter {
            let map: HashMap<String, String> = binds.iter().cloned().collect();
            self.renames.push(map);
            let text = self.render_to_string(f);
            self.renames.pop();
            test = format!("{test} and ({text})");
        }

        if *negated {
            test = format!("not ({test})");
        }

        (decls, test, binds)
    }

    fn if_with_locals(&mut self, span: TokSpan, i: &If) {
        let start = self.byte_start(span);
        let mut cursor = start;
        // Every block this rewrite opens beyond the source `if`'s own `end`.
        let mut extra_ends = 0usize;
        // `if not local x = e then return end` is the guard clause: `x`
        // stays in scope after it, so no `do` wraps the declaration.
        let guard_clause = i.branches.len() == 1
            && i.else_block.is_none()
            && matches!(i.branches[0].0, Cond::Local { negated: true, .. });

        for (idx, (cond, block)) in i.branches.iter().enumerate() {
            // The keyword token before the condition.
            let kw_tok = self.toks[cond.span().start as usize - 1];
            self.copy(cursor, kw_tok.start);
            let then_tok = self.find_tok_after(cond.span().end, "then");
            let then_end = self.toks[then_tok as usize].end;

            match cond {
                Cond::Expr(e) => {
                    let keyword = if idx == 0 { "if" } else { "elseif" };
                    let c = self.render_to_string(e);
                    self.generate(kw_tok.start, &format!("{keyword} {c} then"));
                }

                Cond::Local { .. } => {
                    let (decls, test, binds) = self.cond_local_parts(cond);
                    let lead = if guard_clause {
                        ""
                    } else if idx == 0 {
                        "do "
                    } else {
                        "else do "
                    };

                    // `do` and `if` open two blocks; the source `end` closes
                    // one of them, except in the guard clause, which opens
                    // only the `if`.
                    if !guard_clause {
                        extra_ends += 1;
                    }

                    if idx > 0 {
                        extra_ends += 1;
                    }

                    let mut text = format!("{lead}{} if {test} then", decls.join(" "));

                    if !binds.is_empty() && !matches!(cond, Cond::Local { negated: true, .. }) {
                        let names: Vec<String> = binds.iter().map(|(n, _)| n.clone()).collect();
                        let values: Vec<String> = binds.iter().map(|(_, v)| v.clone()).collect();
                        text.push_str(&format!(
                            " local {} = {}",
                            names.join(", "),
                            values.join(", ")
                        ));
                    }

                    self.generate(kw_tok.start, &text);
                }
            }

            cursor = then_end;
            let body_start = self.block_start_or(block, cursor);
            self.copy(cursor, body_start);
            self.block(block);
            cursor = self.block_end_or(block, body_start);
        }

        if let Some(e) = &i.else_block {
            let else_tok = self.toks[e.span.start as usize - 1];
            let else_tok = if e.span.is_empty() {
                self.toks[span.end as usize - 2]
            } else {
                else_tok
            };
            self.copy(cursor, else_tok.end);
            cursor = else_tok.end;
            let body_start = self.block_start_or(e, cursor);
            self.copy(cursor, body_start);
            self.block(e);
            cursor = self.block_end_or(e, body_start);
        }

        let end_tok = self.toks[span.end as usize - 1];
        self.copy(cursor, end_tok.start);
        self.copy(end_tok.start, end_tok.end);
        self.generate(end_tok.end, &" end".repeat(extra_ends));
    }

    fn while_with_local(&mut self, span: TokSpan, w: &While) {
        let start = self.byte_start(span);
        let (decls, test, binds) = self.cond_local_parts(&w.cond);
        let do_tok = self.find_tok_after(w.cond.span().end, "do");
        let do_end = self.toks[do_tok as usize].end;
        let mut text = format!(
            "while true do {} if not ({test}) then break end",
            decls.join(" ")
        );

        if !binds.is_empty() {
            let names: Vec<String> = binds.iter().map(|(n, _)| n.clone()).collect();
            let values: Vec<String> = binds.iter().map(|(_, v)| v.clone()).collect();
            text.push_str(&format!(
                " local {} = {}",
                names.join(", "),
                values.join(", ")
            ));
        }

        self.generate(start, &text);
        let body_start = self.block_start_or(&w.block, do_end);
        self.copy(do_end, body_start);
        self.block(&w.block);
        let after = self.block_end_or(&w.block, body_start);
        let end_tok = self.toks[span.end as usize - 1];
        self.copy(after, end_tok.start);
        self.copy(end_tok.start, end_tok.end);
    }

    /// `if local x = f() then a else b` as an expression: the binding hoists,
    /// and a pattern's names substitute.
    fn if_expr_with_locals(&mut self, span: TokSpan, branches: &[(Cond, Expr)], else_value: &Expr) {
        let anchor = self.byte_start(span);
        let mut text = String::from("(");

        for (idx, (cond, value)) in branches.iter().enumerate() {
            let keyword = if idx == 0 { "if" } else { "elseif" };

            match cond {
                Cond::Expr(e) => {
                    let c = self.render_to_string(e);
                    let v = self.render_to_string(value);
                    text.push_str(&format!("{keyword} {c} then {v} "));
                }

                Cond::Local { .. } => {
                    let (decls, test, binds) = self.cond_local_parts(cond);

                    for d in decls {
                        // `local x = e` hoists as a temp-like declaration.
                        let d = d.strip_prefix("local ").unwrap_or(&d).to_string();
                        let (name, value) = d
                            .split_once(" = ")
                            .map(|(a, b)| (a.to_string(), b.to_string()))
                            .unwrap_or((d.clone(), "nil".to_string()));
                        let name = name.split(':').next().unwrap_or(&name).trim().to_string();
                        self.hoists.push(Hoist::Stmt {
                            text: format!("local {name} = {value}"),
                            anchor,
                        });
                    }

                    let map: HashMap<String, String> = binds.iter().cloned().collect();
                    self.renames.push(map);
                    let v = self.render_to_string(value);
                    self.renames.pop();
                    text.push_str(&format!("{keyword} {test} then {v} "));
                }
            }
        }

        let e = self.render_to_string(else_value);
        text.push_str(&format!("else {e})"));
        self.generate(anchor, &text);
    }
}

impl<'s> Desugar<'s> {
    // --- prescan -------------------------------------------------------------

    /// Gathers the names that other statements route through: extension
    /// methods, statics, macros, traits. One pass over the top level.
    /// Finds every `reduce` whose initial value is a literal and whose
    /// function leaves its accumulator untyped: the parameter takes the
    /// literal's type, since the checker reads the function before the
    /// value and would leave it generic.
    fn scan_reduce_inserts(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.scan_children(stmt_children(stmt));
        }
    }

    fn scan_children(&mut self, children: Vec<Child<'_>>) {
        for child in children {
            match child {
                Child::Expr(e) => self.scan_expr_for_reduce(e),

                Child::Block(b) => self.scan_reduce_inserts(b),

                Child::Function(f) => self.scan_reduce_inserts(&f.block),
            }
        }
    }

    /// Walks the file for checks the emit does not need. A statement that
    /// desugars to itself is copied whole, so its expressions never reach
    /// `expr`; this pass sees them all.
    fn scan_static_checks(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.check_stmt_attrs(stmt);
            self.check_children_of(stmt_children(stmt));
        }
    }

    /// The attributes of one declaration, against the target each one
    /// takes.
    fn check_stmt_attrs(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Function(f) => self.check_attrs(&f.attrs, "function"),

            Stmt::LocalFunction(f) => self.check_attrs(&f.attrs, "function"),

            Stmt::Local(l) => self.check_attrs(&l.attrs, "local"),

            Stmt::Remote(r) => self.check_attrs(&r.attributes, "remote"),

            Stmt::Struct(st) => {
                self.check_attrs(&st.attributes, "struct");
                let name = self.text_of(st.name).to_string();
                let mut seen: HashSet<String> = HashSet::new();

                for f in &st.fields {
                    self.check_attrs(&f.attributes, "field");
                    let fname = self.text_of(f.name).to_string();

                    if !seen.insert(fname.clone()) {
                        let message = format!("`{name}` declares the field `{fname}` twice");
                        self.diagnose(f.name, &message);
                    }
                }
            }

            Stmt::Enum(e) => {
                self.check_attrs(&e.attributes, "enum");

                for v in &e.variants {
                    self.check_attrs(&v.attributes, "variant");
                }
            }

            _ => {}
        }
    }

    /// One attribute list: every name is declared, takes this target, and
    /// carries the arguments it declares.
    fn check_attrs(&mut self, attrs: &[Attr], target: &str) {
        let mut inline: Option<TokSpan> = None;
        let mut noinline: Option<TokSpan> = None;

        for a in attrs {
            let Some(n) = a.name else { continue };
            let name = self.text_of(n).to_string();

            match name.as_str() {
                "inline" => inline = Some(a.span),
                "noinline" => noinline = Some(a.span),
                _ => {}
            }

            let declared = self.attr_decls.get(&name).cloned();
            let targets: Vec<String> = match (builtin_attr_targets(&name), &declared) {
                (Some(t), _) => t.iter().map(|s| (*s).to_string()).collect(),

                (None, Some((t, _))) => t.clone(),

                (None, None) => {
                    // An imported attribute keeps its targets in the
                    // module that declares it.
                    if !self.imported_names.contains(&name) {
                        let message = format!(
                            "no attribute named `{name}`; `attribute {name} on {target}` declares one"
                        );
                        self.diagnose(a.span, &message);
                    }

                    continue;
                }
            };

            if !targets.iter().any(|t| t == target) {
                let list: Vec<&str> = targets.iter().map(String::as_str).collect();
                let message = format!(
                    "the attribute `{name}` has no meaning on a {target}; it goes on {}",
                    list_names(&list)
                );
                self.diagnose(a.span, &message);

                continue;
            }

            self.check_attr_args(a, &name, declared.as_ref().map(|(_, p)| p.as_slice()));
        }

        if let (Some(_), Some(at)) = (inline, noinline) {
            self.diagnose(
                at,
                "the attributes `inline` and `noinline` ask for opposite things; keep one",
            );
        }
    }

    /// The arguments of one attribute: the count a built-in takes, and the
    /// count and the literal types a declared one takes.
    fn check_attr_args(
        &mut self,
        a: &Attr,
        name: &str,
        params: Option<&[(String, Option<String>)]>,
    ) {
        const NO_ARGS: &[&str] = &[
            "native",
            "checked",
            "inline",
            "noinline",
            "test",
            "sealed",
            "skip",
            "unreliable",
            "immediate",
            "u8",
            "u16",
            "u32",
            "i8",
            "i16",
            "i32",
            "f32",
        ];

        if NO_ARGS.contains(&name) && !a.args.is_empty() {
            let message = format!("the attribute `{name}` takes no argument");
            self.diagnose(a.span, &message);

            return;
        }

        if name == "deprecated" && a.args.len() > 1 {
            let message = format!(
                "the attribute `deprecated` takes one message, {} given",
                a.args.len()
            );
            self.diagnose(a.span, &message);

            return;
        }

        let Some(params) = params else {
            return;
        };

        if a.args.len() != params.len() {
            let message = format!(
                "the attribute `{name}` takes {} argument{}, {} given",
                params.len(),
                if params.len() == 1 { "" } else { "s" },
                a.args.len()
            );
            self.diagnose(a.span, &message);

            return;
        }

        for (arg, (pname, ty)) in a.args.iter().zip(params) {
            let Some(want) = ty.as_deref() else { continue };
            let Some(got) = literal_kind(arg) else {
                continue;
            };

            if want != got {
                let message =
                    format!("the attribute `{name}` takes {want} for `{pname}`, {got} given");
                self.diagnose(arg.span(), &message);
            }
        }
    }

    fn check_children_of(&mut self, children: Vec<Child<'_>>) {
        for child in children {
            match child {
                Child::Expr(e) => {
                    self.check_enum_member(e);
                    self.check_await(e);
                    self.check_children_of(expr_children(e));
                }

                Child::Block(b) => self.scan_static_checks(b),

                Child::Function(f) => self.scan_static_checks(&f.block),
            }
        }
    }

    /// `await 5`: the operand is a literal no Future can be.
    fn check_await(&mut self, e: &Expr) {
        let Expr::Await { operand, span } = e else {
            return;
        };
        let Some(kind) = literal_kind(operand) else {
            return;
        };
        let message = format!("`await` takes a Future or an awaitable, not a {kind}");
        self.diagnose(*span, &message);
    }

    /// `Shape.Triangle` on an enum the file declares: the member is a
    /// variant, a method the impl writes, or nothing at all.
    fn check_enum_member(&mut self, e: &Expr) {
        const BUILT_IN: &[&str] = &["is", "clone", "__index", "__tostring", "__eq", "__call"];

        let Expr::Index {
            object,
            key: IndexKey::Field(field),
            ..
        } = e
        else {
            return;
        };
        let Expr::Name(n) = object.as_ref() else {
            return;
        };
        let ename = self.text_of(*n).to_string();
        let Some(variants) = self.enum_decls.get(&ename) else {
            return;
        };
        let member = self.text_of(*field).to_string();

        if BUILT_IN.contains(&member.as_str())
            || variants.iter().any(|(v, _)| *v == member)
            || self
                .impl_methods
                .get(&ename)
                .is_some_and(|m| m.contains(&member))
        {
            return;
        }

        let names: Vec<&str> = variants.iter().map(|(v, _)| v.as_str()).collect();
        let message = format!(
            "`{ename}` has no variant `{member}`; its variants are {}",
            list_names(&names)
        );
        self.diagnose(*field, &message);
    }

    fn scan_expr_for_reduce(&mut self, e: &Expr) {
        if let Expr::Call {
            method: Some(m),
            args: CallArgs::Paren(args),
            ..
        } = e
            && self.text_of(*m) == "reduce"
            && let [Expr::Function { body, .. }, init] = args.as_slice()
            && body
                .params
                .first()
                .is_some_and(|p| p.ty.is_none() && p.destructure.is_none() && !p.is_vararg)
            && let Some(ty) = literal_type(init)
        {
            let at = self.byte_end(body.params[0].name);
            self.inserts.push((at, format!(": {ty}")));
        }

        self.scan_children(expr_children(e));
    }

    fn prescan(&mut self, block: &Block) {
        for ext in &self.options.extensions {
            if ext.is_static {
                self.ext_statics
                    .entry(ext.target.clone())
                    .or_default()
                    .insert(ext.name.clone());
            } else {
                if PRIMITIVES.contains(&ext.target.as_str()) {
                    self.ext_primitive
                        .insert(ext.name.clone(), ext.target.clone());
                }

                self.ext_methods.insert(ext.name.clone());
            }
        }

        for stmt in &block.stmts {
            let returns_result = |body: &FunctionBody| {
                body.is_async.is_some()
                    && body
                        .ret_type
                        .is_some_and(|rt| self.text_of(rt).trim_start().starts_with("Result<"))
            };

            match stmt {
                Stmt::Function(f) if f.path.len() == 1 && returns_result(&f.body) => {
                    let name = self.text_of(f.path[0]).to_string();
                    self.result_asyncs.insert(name);
                }

                Stmt::LocalFunction(f) if returns_result(&f.body) => {
                    let name = self.text_of(f.name).to_string();
                    self.result_asyncs.insert(name);
                }

                _ => {}
            }

            if let Stmt::TypeAlias(t) = stmt
                && let Some((_, value)) = self.text_of(t.span).split_once('=')
                && value.trim_start().starts_with("Result")
            {
                self.result_aliases.insert(self.text_of(t.name).to_string());
            }

            let declared = match stmt {
                Stmt::TypeAlias(t) => Some(t.name),

                Stmt::Struct(s) => Some(s.name),

                Stmt::Enum(e) => Some(e.name),

                Stmt::Interface(i) => Some(i.name),

                Stmt::Trait(t) => Some(t.name),

                _ => None,
            };

            if let Some(name) = declared {
                self.declared_types.insert(self.text_of(name).to_string());
            }

            match stmt {
                Stmt::Attribute(a) => {
                    let name = self.text_of(a.name).to_string();
                    let targets: Vec<String> = a
                        .targets
                        .iter()
                        .map(|t| self.text_of(*t).to_string())
                        .collect();
                    let params: Vec<(String, Option<String>)> = a
                        .params
                        .iter()
                        .map(|p| {
                            (
                                self.text_of(p.name).to_string(),
                                p.ty.map(|t| self.text_of(t).trim().to_string()),
                            )
                        })
                        .collect();
                    self.attr_decls.insert(name, (targets, params));
                }

                Stmt::Import(i) => match &i.kind {
                    ImportKind::Namespace(n) => {
                        self.imported_names.insert(self.text_of(*n).to_string());
                    }

                    ImportKind::Both(n, specs) => {
                        self.imported_names.insert(self.text_of(*n).to_string());

                        for sp in specs {
                            let name = sp.alias.unwrap_or(sp.name);
                            self.imported_names.insert(self.text_of(name).to_string());
                        }
                    }

                    ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                        for sp in specs {
                            let name = sp.alias.unwrap_or(sp.name);
                            self.imported_names.insert(self.text_of(name).to_string());
                        }
                    }
                },

                Stmt::Enum(e) => {
                    let name = self.text_of(e.name).to_string();
                    let variants: Vec<(String, usize)> = e
                        .variants
                        .iter()
                        .map(|v| (self.text_of(v.name).to_string(), v.payload.len()))
                        .collect();
                    self.enum_decls.insert(name, variants);
                }

                Stmt::Impl(i) => {
                    let target = self.text_of(i.target).to_string();
                    let names: HashSet<String> = i
                        .methods
                        .iter()
                        .filter_map(|m| m.path.first().map(|n| self.text_of(*n).to_string()))
                        .collect();
                    self.impl_methods
                        .entry(target.clone())
                        .or_default()
                        .extend(names);

                    if i.methods.iter().any(|m| {
                        m.path
                            .first()
                            .is_some_and(|n| self.text_of(*n) == "to_string")
                    }) {
                        self.structs_with_to_string.insert(target.clone());
                    }

                    if i.methods
                        .iter()
                        .any(|m| m.visibility.is_some_and(|v| self.text_of(v) == "private"))
                    {
                        self.private_types.insert(target.clone());
                    }

                    if i.trait_name.is_none()
                        && let Some(ctor) = i.methods.iter().find_map(|m| {
                            m.path
                                .first()
                                .map(|n| self.text_of(*n))
                                .filter(|n| matches!(*n, "new" | "New"))
                        })
                    {
                        self.structs_with_new
                            .insert(target.clone(), ctor.to_string());
                    }

                    if self.is_foreign(&target) {
                        for m in &i.methods {
                            let name = self.text_of(m.path[0]).to_string();
                            let has_self = m
                                .body
                                .params
                                .first()
                                .map(|p| self.text_of(p.name) == "self")
                                .unwrap_or(false);

                            if has_self {
                                if PRIMITIVES.contains(&target.as_str()) {
                                    self.ext_primitive.insert(name.clone(), target.clone());
                                }

                                self.ext_methods.insert(name);
                            } else {
                                self.ext_statics
                                    .entry(target.clone())
                                    .or_default()
                                    .insert(name);
                            }
                        }
                    }
                }

                Stmt::Macro(m) => {
                    let params: Vec<String> = m
                        .params
                        .iter()
                        .filter(|p| !p.is_vararg)
                        .map(|p| self.text_of(p.name).to_string())
                        .collect();
                    let variadic = m.params.iter().any(|p| p.is_vararg);
                    let name = self.text_of(m.name).to_string();
                    let body = self.join_tokens(m.body.span);
                    let tail = m.tail.as_ref().map(|t| self.join_tokens(t.span()));
                    self.macros.insert(
                        name,
                        MacroRef {
                            params,
                            variadic,
                            body,
                            tail,
                        },
                    );
                }

                Stmt::Trait(t) => {
                    let name = self.text_of(t.name).to_string();
                    let defaults = t
                        .methods
                        .iter()
                        .filter(|m| m.body.is_some())
                        .map(|m| self.text_of(m.name).to_string())
                        .collect();
                    let required = t
                        .methods
                        .iter()
                        .filter(|m| m.body.is_none())
                        .map(|m| (self.text_of(m.name).to_string(), m.params.len()))
                        .collect();
                    self.traits.insert(name.clone(), defaults);
                    self.trait_required.insert(name, required);
                }

                Stmt::Struct(st) => {
                    self.note_struct(st);
                }

                _ => {}
            }
        }
    }

    /// A bound with each operator trait routed to the runtime type, unless
    /// the file declares a trait of that name.
    fn resolve_bound(&mut self, bound: &str) -> String {
        const BUILTIN: &[&str] = &[
            "Display",
            "Debug",
            "Clone",
            "Eq",
            "PartialEq",
            "Ord",
            "Add",
            "Sub",
            "Mul",
            "Div",
            "Serialize",
        ];
        let parts: Vec<String> = bound
            .split('&')
            .map(|part| {
                let part = part.trim();
                let name = part.split('<').next().unwrap_or(part).trim();

                if BUILTIN.contains(&name) && !self.traits.contains_key(name) {
                    format!("{}.{part}", self.std())
                } else {
                    part.to_string()
                }
            })
            .collect();

        parts.join(" & ")
    }

    /// The tokens of a span on one line, joined by spaces, comments gone.
    fn join_tokens(&self, span: TokSpan) -> String {
        let mut out = String::new();

        for i in span.start..span.end {
            let tok = self.toks[i as usize];

            if !out.is_empty() {
                out.push(' ');
            }

            out.push_str(tok.text(self.src));
        }

        out
    }

    /// A type that is not an Alloy struct or enum: an engine class, a
    /// datatype, or a primitive. Its metatable cannot take methods.
    fn is_foreign(&self, name: &str) -> bool {
        !self.structs.contains(name)
            && !self.enums.contains_key(name)
            && (INSTANCE_CLASSES.contains(&name)
                || DATATYPES.contains(&name)
                || PRIMITIVES.contains(&name)
                || name == "Instance")
    }

    fn chain_has_ext(&self, e: &Expr) -> bool {
        if self.ext_methods.is_empty() && self.ext_statics.is_empty() {
            return false;
        }

        let (base, links) = flatten(e);

        // The check artifact keeps a call on a class as written; only a
        // primitive needs the helper table rewrite.
        if self.options.check {
            let primitive_method = links.iter().any(|l| match l {
                Link::Plain(Step::Call {
                    method: Some(m), ..
                })
                | Link::Optional(Step::Call {
                    method: Some(m), ..
                }) => self.ext_primitive.contains_key(self.text_of(*m)),

                _ => false,
            });

            if primitive_method {
                return true;
            }

            return matches!(
                (base, links.first(), links.get(1)),
                (
                    Expr::Name(n),
                    Some(Link::Plain(Step::Field(f))),
                    Some(Link::Plain(Step::Call { method: None, .. })),
                ) if PRIMITIVES.contains(&self.text_of(*n))
                    && self
                        .ext_statics
                        .get(self.text_of(*n))
                        .is_some_and(|s| s.contains(self.text_of(*f)))
            );
        }

        if links.iter().any(|l| match l {
            Link::Plain(Step::Call {
                method: Some(m), ..
            })
            | Link::Optional(Step::Call {
                method: Some(m), ..
            }) => self.ext_methods.contains(self.text_of(*m)),

            _ => false,
        }) {
            return true;
        }

        // `Vector3.zero(...)`: a static on a foreign type.
        if let (
            Expr::Name(n),
            Some(Link::Plain(Step::Field(f))),
            Some(Link::Plain(Step::Call { method: None, .. })),
        ) = (base, links.first(), links.get(1))
        {
            let target = self.text_of(*n);

            if let Some(statics) = self.ext_statics.get(target)
                && statics.contains(self.text_of(*f))
            {
                return true;
            }
        }

        false
    }

    // --- structs -------------------------------------------------------------

    /*
    A struct is a class table with `__index`, a raw constructor on the class
    table's own metatable, and a type. Field lines hold nothing at runtime;
    the header carries the tables and the `end` line carries the type and
    the derives. A `.d.aly` keeps only the type.
    */
    fn struct_decl(&mut self, st: &StructDecl) {
        let name = self.text_of(st.name).to_string();
        let start = self.byte_start(st.span);
        let end_tok = self.toks[st.span.end as usize - 1];
        let generics = st
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let export = if st.exported { "export " } else { "" };

        // Field types and defaults.
        let mut field_types = Vec::new();
        // The constructor's record: a field with a default may be left out.
        let mut param_types = Vec::new();
        let mut defaults = Vec::new();
        let mut field_names = Vec::new();
        let mut field_attrs = Vec::new();
        // The check artifact keeps a private field out of the public view.
        let split = self.has_private_view(&name);
        let mut public_types = Vec::new();
        let mut private_types = Vec::new();

        for f in &st.fields {
            let fname = self.text_of(f.name).to_string();
            let ty = self.copy_type_to_string(f.ty);
            let modifier = f
                .modifier
                .map(|m| format!("{} ", self.text_of(m)))
                .unwrap_or_default();
            let opt = if f.default.is_some() && !ty.trim_end().ends_with('?') {
                "?"
            } else {
                ""
            };
            field_types.push(format!("{modifier}{fname}: {ty}"));
            // The constructor fills defaults into this table, so a
            // `read` field is plain here.
            param_types.push(format!("{fname}: {ty}{opt}"));

            if f.visibility.is_some_and(|v| self.text_of(v) == "private") {
                private_types.push(format!("{modifier}{fname}: {ty}"));
            } else {
                public_types.push(format!("{modifier}{fname}: {ty}"));
            }

            if let Some(dv) = &f.default {
                self.expected_generic = generic_head(&ty);
                let v = self.render_to_string(dv);
                self.expected_generic = None;
                defaults.push(format!("if f.{fname} == nil then f.{fname} = {v} end"));
            }

            let attrs = self.attr_table(&f.attributes);

            if attrs != "{}" {
                field_attrs.push(format!("{fname} = {attrs}"));
            }

            field_names.push(fname);
        }

        let type_line = if split {
            // `Name` is what the world sees; `Name__all` is the same table
            // with the private fields and the private methods, which the
            // impl's methods put on `Name__private` in this artifact.
            let hidden = if private_types.is_empty() {
                String::new()
            } else {
                format!(" & {{ {} }}", private_types.join(", "))
            };

            format!(
                "{export}type {name} = typeof(setmetatable({{}} :: {{ {} }}, {name})) type {name}__all = {name}{hidden} & typeof({name}__private)",
                public_types.join(", ")
            )
        } else {
            format!(
                "{export}type {name}{generics} = typeof(setmetatable({{}} :: {{ {} }}, {name}))",
                field_types.join(", ")
            )
        };

        if self.options.definitions {
            self.generate(
                start,
                &format!(
                    "{export}type {name}{generics} = {{ {} }}",
                    field_types.join(", ")
                ),
            );
            self.blank_lines(start, end_tok.end);

            return;
        }

        // Header.
        // The check artifact types the raw constructor, so `new Name { }`
        // is a `Name` to the checker and not a metatable over a literal.
        // A generic struct stays untyped: its parameters are not in
        // scope here. A struct whose impl writes `new` gets no generated
        // one in the check artifact, so the two do not clash.
        // The check artifact has no metatable on the class table: with one,
        // the instance type nests a metatable of its own, and the checker
        // then rejects the struct as a trait or a `Deletable`. The fields
        // form calls `__new`, a typed raw constructor, instead of the
        // class. A generic struct stays untyped, its parameters being out
        // of scope.
        // A generic struct types its constructor too: the parameters go
        // on the function, so the field types and the result name them.
        let typed = self.options.check && (generics.is_empty() || !split);
        let fn_generics = if generics.is_empty() {
            String::new()
        } else {
            generics.clone()
        };
        let (param, ret) = if typed {
            (
                format!("f: {{ {} }}", param_types.join(", ")),
                format!(": {name}{fn_generics}"),
            )
        } else {
            ("f".to_string(), String::new())
        };
        let d = defaults.join(" ");
        let header = if self.options.check {
            let new_fn = if self.structs_with_new.contains_key(&name) {
                String::new()
            } else {
                format!(
                    " function {name}.new{fn_generics}({param}){ret} return {name}.__new(f) end"
                )
            };

            let private_table = if split {
                format!(" local {name}__private = {{}}")
            } else {
                String::new()
            };

            format!(
                "local {name} = {{}} {name}.__index = {name}{private_table} function {name}.__new{fn_generics}({param}){ret} {d} return (setmetatable(f, {name}) :: any) end{new_fn}"
            )
        } else {
            format!(
                "local {name} = {{}} {name}.__index = {name} setmetatable({name}, {{ __call = function(_, f) {d} return setmetatable(f, {name}) end }}) function {name}.new(f) return {name}(f) end"
            )
        };
        self.generate(start, &header);

        // Field lines carry only their trivia. The range starts at the
        // attributes, so an attribute line above `struct` keeps its newline.
        self.blank_lines(start, end_tok.start);

        // Derives and attributes on the `end` line.
        let mut tail = type_line;
        let mut derives_debug = false;
        let mut derived: HashSet<String> = HashSet::new();

        for a in &st.attributes {
            let Some(aname) = a.name else { continue };

            if self.text_of(aname) == "derive" {
                for arg in &a.args {
                    let which = self.text_of(arg.span()).to_string();
                    derives_debug |= which == "Debug";

                    // `Eq` and `PartialEq` write the same `__eq`; naming
                    // both must not write it twice.
                    let key = if which == "PartialEq" {
                        "Eq".to_string()
                    } else {
                        which.clone()
                    };

                    if !derived.insert(key) {
                        continue;
                    }

                    tail.push(' ');
                    tail.push_str(&self.derive_struct(
                        &name,
                        &which,
                        arg.span(),
                        &field_names,
                        &st.fields,
                    ));
                }
            }
        }

        // The default printer: `Name { x = 1, y = 2 }`. A `to_string` in
        // the struct's impl, or `@derive(Debug)`, writes its own and this
        // one stays out; a `__tostring` set later replaces it either way.
        if !derives_debug && !self.structs_with_to_string.contains(&name) {
            let std = self.std();
            let sn = if self.options.check && !self.generic_types.contains(&name) {
                format!(": {}", self.self_alias(&name))
            } else {
                String::new()
            };
            let fields: Vec<String> = field_names.iter().map(|f| luau_string(f)).collect();
            tail.push_str(&format!(
                " {name}.__tostring = function(s{sn}) return {std}.show_struct({}, s, {{ {} }}) end",
                luau_string(&name),
                fields.join(", ")
            ));
        }

        let own = self.attr_table(&st.attributes);

        if own != "{}" || !field_attrs.is_empty() {
            let std = self.std();
            tail.push_str(&format!(
                " {std}.attrs({name}, {{ own = {own}, fields = {{ {} }} }})",
                field_attrs.join(", ")
            ));
        }

        // `@sealed`: a write to a key the struct does not declare raises.
        // Declared keys are present from construction, so `__newindex`
        // only sees a declared key when its value was nil; that write
        // goes through.
        // The check artifact leaves it out: the struct type already
        // rejects an unknown key, and a `__newindex` on the metatable
        // stops the solver from reducing a mapped type over the struct.
        if !self.options.check
            && st
                .attributes
                .iter()
                .any(|a| a.name.is_some_and(|n| self.text_of(n) == "sealed"))
        {
            let keys: Vec<String> = field_names.iter().map(|f| format!("{f} = true")).collect();
            tail.push_str(&format!(
                " {name}.__newindex = function(t, k, v) if ({{ {} }})[k] then rawset(t, k, v) else error(string.format(\"%s has no field %s\", {}, tostring(k)), 2) end end",
                keys.join(", "),
                luau_string(&name)
            ));
        }

        self.generate(end_tok.start, &format!(" {tail}"));

        if st.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// Copies the lines in a range as blank lines, keeping the newlines.
    fn blank_lines(&mut self, start: u32, end: u32) {
        let text = &self.src[start as usize..end as usize];
        let mut cursor = start;

        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                let at = start + i as u32;
                // Nothing before the newline copies; the newline does.
                cursor = at;
                self.copy(cursor, at + 1);
                cursor = at + 1;
            }
        }

        let _ = cursor;
    }

    /// A type span rendered with its edits applied.
    fn copy_type_to_string(&mut self, span: TokSpan) -> String {
        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        self.copy_span(span);
        std::mem::swap(&mut self.r, &mut side);

        side.finish().0
    }

    /// `{ range = { 0, 100 }, skip = {} }` from a list of attributes,
    /// skipping the ones the compiler consumes itself.
    fn attr_table(&mut self, attrs: &[Attr]) -> String {
        let mut parts = Vec::new();

        for a in attrs {
            let Some(n) = a.name else { continue };
            let name = self.text_of(n).to_string();

            if matches!(
                name.as_str(),
                "derive" | "test" | "native" | "checked" | "deprecated" | "cfg"
            ) {
                continue;
            }

            let args: Vec<String> = a.args.iter().map(|e| self.render_to_string(e)).collect();
            parts.push(format!("{name} = {{ {} }}", args.join(", ")));
        }

        format!("{{ {} }}", parts.join(", ")).replace("{  }", "{}")
    }

    fn derive_struct(
        &mut self,
        name: &str,
        which: &str,
        at: TokSpan,
        fields: &[String],
        decls: &[Field],
    ) -> String {
        // The check artifact types the receiver, as an impl method's self.
        let sn = if self.options.check && !self.generic_types.contains(name) {
            format!(": {}", self.self_alias(name))
        } else {
            String::new()
        };
        let tn = if self.options.check { ": any" } else { "" };
        match which {
            "Eq" | "PartialEq" => {
                let cmp: Vec<String> = fields.iter().map(|f| format!("a.{f} == b.{f}")).collect();
                let body = if cmp.is_empty() {
                    "true".to_string()
                } else {
                    cmp.join(" and ")
                };

                format!("{name}.__eq = function(a{sn}, b{sn}) return {body} end")
            }

            // Field by field, in declaration order: the first field that
            // differs decides, as a tuple compares.
            "Ord" => {
                let steps: Vec<String> = fields
                    .iter()
                    .map(|f| format!("if a.{f} ~= b.{f} then return a.{f} < b.{f} end"))
                    .collect();
                let steps = steps.join(" ");

                format!(
                    "{name}.__lt = function(a{sn}, b{sn}) {steps} return false end {name}.__le = function(a{sn}, b{sn}) {steps} return true end"
                )
            }

            "Debug" => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| format!("\"{f} = \" .. tostring(s.{f})"))
                    .collect();
                let inner = if parts.is_empty() {
                    "\"\"".to_string()
                } else {
                    parts.join(" .. \", \" .. ")
                };

                let ret = if self.options.check { ": string" } else { "" };

                format!(
                    "{name}.__tostring = function(s{sn}) return \"{name} {{ \" .. {inner} .. \" }}\" end function {name}.debug(self{sn}){ret} return tostring(self) end"
                )
            }

            "Clone" => {
                // The checker reads `setmetatable` of a metatable type as a
                // new shape; the annotation keeps the struct's own.
                let ret = if self.options.check {
                    format!(": {name}")
                } else {
                    String::new()
                };
                let value = self.any_cast(&format!("setmetatable(table.clone(self), {name})"));

                format!("function {name}.clone(self{sn}){ret} return {value} end")
            }

            "Serialize" => {
                let mut to = Vec::new();
                let mut from = Vec::new();

                for f in decls {
                    let fname = self.text_of(f.name).to_string();
                    let skip = f
                        .attributes
                        .iter()
                        .any(|a| a.name.map(|n| self.text_of(n) == "skip").unwrap_or(false));

                    if skip {
                        continue;
                    }

                    let key = f
                        .attributes
                        .iter()
                        .find(|a| a.name.map(|n| self.text_of(n) == "rename").unwrap_or(false))
                        .and_then(|a| a.args.first())
                        .map(|e| self.text_of(e.span()).trim_matches('"').to_string())
                        .unwrap_or(fname.clone());
                    to.push(format!("{key} = self.{fname}"));
                    from.push(format!("{fname} = t.{key}"));
                }

                format!(
                    "function {name}.to_table(self{sn}) return {{ {} }} end function {name}.from_table(t{tn}) return {}({{ {} }}) end",
                    to.join(", "),
                    self.raw_ctor(name),
                    from.join(", ")
                )
            }

            other => {
                let message = format!("unknown derive `{other}`");
                self.diagnose(at, &message);

                String::new()
            }
        }
    }

    // --- traits --------------------------------------------------------------

    /// A trait is a type of its method signatures plus a table holding the
    /// default bodies, which `impl` copies onto the struct.
    fn trait_decl(&mut self, t: &TraitDecl) {
        let name = self.text_of(t.name).to_string();
        let start = self.byte_start(t.span);
        let end_tok = self.toks[t.span.end as usize - 1];
        let export = if t.exported { "export " } else { "" };
        let sigs: Vec<String> = t
            .methods
            .iter()
            .map(|m| self.trait_method_type(m))
            .collect();
        // Methods are read properties: a struct's methods are read-only to
        // the checker, and a read-write slot would reject them.
        let props: Vec<String> = sigs.iter().map(|s| format!("read {s}")).collect();
        let type_line = format!("{export}type {name} = {{ {} }}", props.join(", "));

        if self.options.definitions {
            self.generate(start, &type_line);
            self.blank_lines(self.byte_end(t.name), end_tok.end);

            return;
        }

        // A signature without a body is a typed nil on the table, so
        // `Trait.` completes and hovers it. Assigning nil adds no key, so
        // the runtime table holds the default bodies only.
        let stubs: String = t
            .methods
            .iter()
            .filter(|m| m.body.is_none())
            .map(|m| {
                let sig = self.trait_method_type(m);
                let ty = sig.split_once(": ").map(|(_, t)| t).unwrap_or("any");
                let mname = self.text_of(m.name);

                format!(" {name}.{mname} = (nil :: any) :: {ty}")
            })
            .collect();
        self.generate(start, &format!("local {name} = {{}} {type_line}{stubs}"));
        let mut cursor = self.byte_end(t.name);

        for m in &t.methods {
            let ms = self.byte_start(m.span);
            self.blank_lines(cursor, ms);

            match &m.body {
                Some(body) => {
                    // `function name(params): R` becomes `function Trait.name(params): R`.
                    let mname = self.text_of(m.name).to_string();
                    let mut sig = self.signature_text(m.signature);

                    // An untyped `self` is `any` in the check artifact: the
                    // default lands on every implementing struct.
                    if self.options.check {
                        if sig.starts_with("(self)") {
                            sig = sig.replacen("(self)", "(self: any)", 1);
                        } else if sig.starts_with("(self,") {
                            sig = sig.replacen("(self,", "(self: any,", 1);
                        }
                    }

                    self.generate(ms, &format!("function {name}.{mname}{sig}"));
                    let body_start = self.block_start_or(&body.block, self.byte_end(m.span));
                    self.copy(self.byte_end(m.signature), body_start);
                    self.block(&body.block);
                    let after = self.block_end_or(&body.block, body_start);
                    self.copy(after, self.byte_end(m.span));
                }

                None => self.blank_lines(ms, self.byte_end(m.span)),
            }

            cursor = self.byte_end(m.span);
        }

        self.blank_lines(cursor, end_tok.end);

        if t.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// A signature span with `->` written as `:`.
    fn signature_text(&mut self, sig: TokSpan) -> String {
        let mut out = String::new();

        for i in sig.start..sig.end {
            let tok = self.toks[i as usize];
            let text = tok.text(self.src);

            if i > sig.start {
                let prev = self.toks[i as usize - 1];
                out.push_str(&self.src[prev.end as usize..tok.start as usize]);
            }

            out.push_str(if text == "->" { ":" } else { text });
        }

        out
    }

    fn trait_method_type(&mut self, m: &TraitMethod) -> String {
        let mname = self.text_of(m.name).to_string();
        let mut params = Vec::new();

        for p in &m.params {
            let pname = self.text_of(p.name).to_string();

            if pname == "self" {
                params.push("self: any".to_string());
            } else if p.is_vararg {
                let ty =
                    p.ty.map(|t| self.copy_type_to_string(t))
                        .unwrap_or("any".to_string());
                params.push(format!("...{ty}"));
            } else {
                let ty =
                    p.ty.map(|t| self.copy_type_to_string(t))
                        .unwrap_or("any".to_string());
                params.push(format!("{pname}: {ty}"));
            }
        }

        // The return type follows the `)` in the signature.
        let sig = self.text_of(m.signature).to_string();
        let ret = sig
            .rsplit_once(')')
            .map(|(_, r)| r.trim())
            .map(|r| {
                r.trim_start_matches("->")
                    .trim_start_matches(':')
                    .trim()
                    .to_string()
            })
            .filter(|r| !r.is_empty())
            .unwrap_or("()".to_string());

        format!("{mname}: ({}) -> {ret}", params.join(", "))
    }

    // --- interfaces ------------------------------------------------------------

    fn interface_decl(&mut self, i: &InterfaceDecl) {
        let name = self.text_of(i.name).to_string();
        let start = self.byte_start(i.span);
        let end_tok = self.toks[i.span.end as usize - 1];
        let export = if i.exported { "export " } else { "" };
        let generics = i
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let mut parts: Vec<String> = i
            .extends
            .iter()
            .map(|b| self.text_of(*b).to_string())
            .collect();
        let fields: Vec<String> = i
            .fields
            .iter()
            .map(|f| {
                let modifier = f
                    .modifier
                    .map(|m| format!("{} ", self.text_of(m)))
                    .unwrap_or_default();
                let fname = self.text_of(f.name).to_string();
                let ty = self.copy_type_to_string(f.ty);

                format!("{modifier}{fname}: {ty}")
            })
            .collect();
        parts.push(format!("{{ {} }}", fields.join(", ")));
        self.generate(
            start,
            &format!("{export}type {name}{generics} = {}", parts.join(" & ")),
        );
        self.blank_lines(self.byte_end(i.name), end_tok.end);
    }

    // --- remotes -----------------------------------------------------------

    fn remote_decl(&mut self, r: &RemoteDecl) {
        let name = self.text_of(r.name).to_string();
        let start = self.byte_start(r.span);
        let end = self.byte_end(r.span);

        if self.options.definitions {
            self.blank_lines(start, end);

            return;
        }

        for p in &r.params {
            let Some(ty) = p.ty else { continue };
            let text = self.text_of(ty).to_string();

            if let Some((field, bad, why)) = wire_offender(&text) {
                let pname = self.text_of(p.name).to_string();
                let what = match field {
                    Some(f) => format!("parameter `{pname}` has field `{f}` of type `{bad}`"),

                    None => format!("parameter `{pname}` has type `{bad}`"),
                };
                self.diagnose(
                    ty,
                    &format!("remote `{name}`: {what}, which {why}; a remote carries only data"),
                );
            }
        }

        let params: Vec<String> = r
            .params
            .iter()
            .map(|p| luau_string(self.text_of(p.name)))
            .collect();
        let defaults: Vec<String> = r
            .params
            .iter()
            .filter_map(|p| {
                p.default.as_ref().map(|d| {
                    let v = self.render_to_string(d);

                    format!("{} = {v}", self.text_of(p.name))
                })
            })
            .collect();
        let attrs = self.attr_table(&r.attributes);
        // The layout names locals the check artifact need not resolve,
        // and the checker types the remote by its declaration.
        let wire = self.wire_layout(r);
        let wire = if self.options.check || wire.iter().all(Wire::is_any) {
            String::new()
        } else {
            let kinds: Vec<String> = wire.iter().map(Wire::luau).collect();

            format!(", wire = {{ {} }}", kinds.join(", "))
        };
        let kind = if r.is_function { "function" } else { "event" };
        let std = self.std();
        let value = format!(
            "{std}.remote({{ name = {}, kind = \"{kind}\", from_client = {}, from_server = {}, params = {{ {} }}, defaults = {{ {} }}, attrs = {attrs}{wire} }})",
            luau_string(&name),
            r.from_client,
            r.from_server,
            params.join(", "),
            defaults.join(", ")
        );
        // The check artifact types the object by the declaration, so a
        // handler's parameters and a `fire` carry the declared types.
        let text = if self.options.check {
            let ty = self.remote_type(r);

            format!("local {name} = (({value} :: any) :: {ty})")
        } else {
            format!("local {name} = {value}")
        };
        self.generate(start, &text);
        self.blank_lines(start, end);

        if r.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// The wire layout of each parameter: a width attribute, `@u8`, on a
    /// number; else from the type, down into a struct, a table type, or
    /// an array of those; `any` for what crosses as it is. A `?` type or
    /// a default marks one that may be nil.
    fn wire_layout(&mut self, r: &RemoteDecl) -> Vec<Wire> {
        let mut kinds = Vec::new();
        let mut from = self.byte_end(r.name);

        for p in &r.params {
            let gap = self.src[from as usize..self.byte_start(p.name) as usize].to_string();
            from = self.byte_end(p.name);
            let width: Option<String> = gap.find('@').map(|at| {
                gap[at + 1..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect()
            });
            let ty =
                p.ty.map(|t| self.text_of(t).trim().to_string())
                    .unwrap_or_else(|| "any".to_string());
            let width = width.filter(|w| WIRE_WIDTHS.contains(&w.as_str()));

            if let Some(w) = &width {
                let base = ty.trim_end_matches('?').trim();

                if base != "number" && base != "any" {
                    let pname = self.text_of(p.name).to_string();
                    self.diagnose(
                        p.name,
                        &format!("`@{w}` packs a `number`; parameter `{pname}` is `{base}`"),
                    );
                }
            }

            let wire = self
                .wire_of_type(&ty, width.as_deref(), 0)
                .with_optional(p.default.is_some());
            kinds.push(wire);
        }

        kinds
    }

    /// The layout of one type text. A struct declared here or in the
    /// project opens to its fields; a record type to its members; `T[]`,
    /// `{ T }`, and `Array<T>` to their item. Anything else is `any`.
    fn wire_of_type(&self, text: &str, width: Option<&str>, depth: usize) -> Wire {
        let mut ty = text.trim();
        let mut optional = false;

        while let Some(inner) = ty.strip_suffix('?') {
            ty = inner.trim();
            optional = true;
        }

        while ty.starts_with('(') && ty.ends_with(')') && group_len(ty, '(', ')') == Some(ty.len())
        {
            ty = ty[1..ty.len() - 1].trim();
        }

        let scalar = |kind: &str| Wire::Scalar {
            kind: kind.to_string(),
            optional,
        };

        if let Some(w) = width
            && (ty == "number" || ty == "any")
        {
            return scalar(w);
        }

        if depth > 6 {
            return scalar("any");
        }

        match ty {
            "number" => return scalar("f64"),
            "boolean" => return scalar("bool"),
            "string" => return scalar("str"),
            _ => {}
        }

        let array = |item: &str| Wire::Array {
            item: Box::new(self.wire_of_type(item, None, depth + 1)),
            optional,
        };

        if let Some(item) = ty.strip_suffix("[]") {
            return array(item);
        }

        for head in ["Array<", "ReadArray<", "WriteArray<"] {
            if let Some(rest) = ty.strip_prefix(head)
                && let Some(item) = rest.strip_suffix('>')
            {
                return array(item);
            }
        }

        if let Some(inner) = ty.strip_prefix('{')
            && let Some(inner) = inner.strip_suffix('}')
        {
            let inner = inner.trim();
            let parts = split_top_level(inner, ',');

            if parts.len() == 1 && !inner.contains(':') && !inner.starts_with('[') {
                return array(inner);
            }

            let mut fields = Vec::new();

            for part in parts {
                let part = part.trim();
                let part = part
                    .strip_prefix("read ")
                    .or_else(|| part.strip_prefix("write "))
                    .unwrap_or(part)
                    .trim();

                if part.is_empty() {
                    continue;
                }

                let Some(colon) = part.find(':') else {
                    return scalar("any");
                };
                let name = part[..colon].trim();

                if name.starts_with('[') || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return scalar("any");
                }

                fields.push((
                    name.to_string(),
                    self.wire_of_type(&part[colon + 1..], None, depth + 1),
                ));
            }

            if fields.is_empty() {
                return scalar("any");
            }

            return Wire::Table {
                fields,
                struct_name: None,
                optional,
            };
        }

        if ty.chars().all(|c| c.is_alphanumeric() || c == '_') {
            let declared = self.struct_wire.get(ty).cloned().or_else(|| {
                self.options
                    .shapes
                    .iter()
                    .find(|sh| sh.name == ty)
                    .map(|sh| sh.fields.clone())
            });

            if let Some(declared) = declared {
                let fields = declared
                    .iter()
                    .map(|f| {
                        (
                            f.name.clone(),
                            self.wire_of_type(&f.ty, f.width.as_deref(), depth + 1),
                        )
                    })
                    .collect();

                return Wire::Table {
                    fields,
                    struct_name: Some(ty.to_string()),
                    optional,
                };
            }
        }

        scalar("any")
    }

    /// The type of a remote object, from its declaration. The side that
    /// fires passes the parameters, with a default making one optional;
    /// the side that handles gets them filled, and the server's handler
    /// gets the sender first. A remote open on both sides overloads.
    fn remote_type(&mut self, r: &RemoteDecl) -> String {
        let std = self.std();
        let mut fire_params = Vec::new();
        let mut handler_params = Vec::new();

        for p in &r.params {
            let name = self.text_of(p.name).to_string();
            let ty =
                p.ty.map(|t| self.text_of(t).trim().to_string())
                    .unwrap_or_else(|| "any".to_string());
            let optional = if p.default.is_some() && !ty.ends_with('?') {
                format!("{ty}?")
            } else {
                ty.clone()
            };
            fire_params.push(format!("{name}: {optional}"));
            handler_params.push(format!("{name}: {ty}"));
        }

        let fire = fire_params.join(", ");
        let handler = handler_params.join(", ");
        let with_player = |first: &str, rest: &str| {
            if rest.is_empty() {
                first.to_string()
            } else {
                format!("{first}, {rest}")
            }
        };
        let ret = r
            .ret_type
            .map(|t| self.text_of(t).trim().to_string())
            .unwrap_or_else(|| "()".to_string());
        // A server handler of a remote function may answer with a Future.
        // `Future<()>` is no type, so an event's `call` yields any.
        let answer = if r.is_function && ret != "()" {
            format!("{ret} | {std}.Future<{ret}>")
        } else {
            ret.clone()
        };
        let future = if r.is_function && ret != "()" {
            format!("{std}.Future<{ret}>")
        } else {
            format!("{std}.Future<any>")
        };
        let connection = if r.is_function {
            "()"
        } else {
            "RBXScriptConnection"
        };

        // The client fires and the server handles.
        let client_fire = format!("({fire}) -> ()");
        let server_on = format!(
            "(handler: ({}) -> {answer}) -> {connection}",
            with_player("sender: Player", &handler)
        );
        let client_call = format!("({fire}) -> {future}");
        // The server fires and the client handles.
        let server_fire = format!("({}) -> ()", with_player("player: Player", &fire));
        let client_on = format!("(handler: ({handler}) -> {ret}) -> {connection}");
        let server_call = format!("({}) -> {future}", with_player("player: Player", &fire));

        let pick = |client: String, server: String| match (r.from_client, r.from_server) {
            (true, false) => client,
            (false, true) => server,
            _ => format!("({client}) & ({server})"),
        };
        // A `.client.aly` or `.server.aly` file sees one side of the
        // remote. Every other file is shared and sees both, as a module
        // that branches on `RunService` does.
        let side = file_side(&self.options.file_name);
        let client_fires = r.from_client && side != Some(Side::Server);
        let server_fires = r.from_server && side != Some(Side::Client);
        let client_handles = r.from_server && side != Some(Side::Server);
        let server_handles = r.from_client && side != Some(Side::Client);
        let mut members = vec!["spec: any".to_string(), "instance: Instance?".to_string()];

        match (client_fires, server_fires) {
            (false, false) => {}

            (a, b) => {
                let ty = match (a, b) {
                    (true, false) => client_fire,
                    (false, true) => server_fire,
                    _ => pick(client_fire, server_fire),
                };
                let call = match (a, b) {
                    (true, false) => client_call,
                    (false, true) => server_call,
                    _ => pick(client_call, server_call),
                };
                members.push(format!("fire: {ty}"));
                members.push(format!("call: {call}"));
            }
        }

        // Only the server reaches every client.
        if server_fires {
            members.push(format!("fire_all: ({fire}) -> ()"));
            members.push(format!(
                "fire_except: ({}) -> ()",
                with_player("except: Player", &fire)
            ));
        }

        if client_handles || server_handles {
            let ty = match (client_handles, server_handles) {
                (true, false) => client_on,
                (false, true) => server_on,
                _ => pick(server_on, client_on),
            };
            members.push(format!("on: {ty}"));
            members.push(format!("once: {ty}"));
            members.push(format!("wait: () -> {std}.Future<any>"));
        }

        // The rate limit guards the client-to-server direction, so only
        // the server hears a sender it refused.
        if server_handles {
            members.push("on_ratelimited: (handler: (player: Player) -> ()) -> ()".to_string());
        }

        format!("{{ {} }}", members.join(", "))
    }

    // --- attributes ------------------------------------------------------------

    fn attribute_decl(&mut self, a: &AttributeDecl) {
        let name = self.text_of(a.name).to_string();
        let start = self.byte_start(a.span);

        if self.options.definitions {
            self.blank_lines(start, self.byte_end(a.span));

            return;
        }

        let targets: Vec<String> = a
            .targets
            .iter()
            .map(|t| luau_string(self.text_of(*t)))
            .collect();
        let params: Vec<String> = a
            .params
            .iter()
            .map(|p| luau_string(self.text_of(p.name)))
            .collect();
        let std = self.std();
        let value = format!(
            "{std}.attribute({}, {{ {} }}, {{ {} }})",
            luau_string(&name),
            targets.join(", "),
            params.join(", ")
        );
        // The check artifact types the value by its arguments, so
        // `Attributes.get(S, attr)` reads as `{ min: number, max: number }?`.
        let value = if self.options.check {
            let fields: Vec<String> = a
                .params
                .iter()
                .map(|p| {
                    let ty = match p.ty {
                        Some(t) => self.copy_type_to_string(t),

                        None => "any".to_string(),
                    };

                    format!("{}: {}", self.text_of(p.name), ty.trim())
                })
                .collect();

            format!(
                "({value} :: any) :: {std}.Attribute<{{ {} }}>",
                fields.join(", ")
            )
        } else {
            value
        };
        self.generate(start, &format!("local {name} = {value}"));
        self.blank_lines(start, self.byte_end(a.span));

        if a.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /*
    A function with attributes. Upstream attributes stay in front of it,
    in the bracket form when they carry arguments. `@test` makes the
    function local, registers it for the runner, and blanks it from the
    ship artifact. User attributes attach to the function value after its
    `end`, on the same line.
    */
    fn attributed_function(
        &mut self,
        span: TokSpan,
        attrs: &[Attr],
        body: &FunctionBody,
        name: Option<TokSpan>,
        exported: bool,
        is_local: bool,
    ) {
        let start = self.byte_start(span);
        let mut upstream = Vec::new();
        let mut user = Vec::new();
        let mut is_test = false;
        let mut cfg = None;

        for a in attrs {
            match a.name.map(|n| self.text_of(n)) {
                Some("test") => is_test = true,

                Some("cfg") => match self.cfg_condition(&a.args) {
                    Ok(cond) => cfg = Some((cond, self.text_of(a.span).to_string())),

                    Err(message) => self.diagnose(a.span, &message),
                },

                // Luau has no `@inline` or `@noinline`; the emit would
                // report an invalid attribute on the declaration's line.
                Some("inline" | "noinline") => {}

                // The count check reports on the attribute; the emit
                // would report again, on the declaration's line.
                Some("deprecated") if a.args.len() > 1 => {}

                Some(n @ ("native" | "checked" | "deprecated")) => {
                    if a.args.is_empty() {
                        upstream.push(format!("@{n}"));
                    } else {
                        let args: Vec<String> =
                            a.args.iter().map(|e| self.render_to_string(e)).collect();
                        upstream.push(format!("@[{n}({})]", args.join(", ")));
                    }
                }

                None => upstream.push(self.text_of(a.span).to_string()),

                Some(n) => {
                    let args: Vec<String> =
                        a.args.iter().map(|e| self.render_to_string(e)).collect();
                    user.push(format!("{n} = {{ {} }}", args.join(", ")));
                }
            }
        }

        // The declaration starts after the attributes.
        let first_tok = attrs.last().map(|a| a.span.end).unwrap_or(span.start);
        let decl_start = self.toks[first_tok as usize].start;
        let fname = name.map(|n| self.text_of(n).to_string());

        if is_test && !self.options.tests {
            self.ship_blanks.push((start, self.byte_end(span)));
        }

        let mut lead = upstream.join(" ");

        if !lead.is_empty() {
            lead.push(' ');
        }

        if (is_test || exported) && !is_local {
            lead.push_str("local ");
        }

        // The lead sits on the declaration's line, so the function and
        // its fold start there. An attribute on its own line keeps that
        // line, blank.
        self.blank_lines(start, decl_start);
        self.generate(decl_start, &lead);
        let rest = TokSpan::new(first_tok as usize, span.end as usize);

        // `@cfg(server)`: the function stays, typed as written, and its
        // body opens with the check. A shared module loads on both
        // sides, so the condition is read when the function runs.
        if let Some((cond, text)) = cfg {
            // An empty body puts the check behind the header; a body
            // with statements puts it before the first.
            let (at, pad) = if body.block.span.is_empty() {
                (self.toks[span.end as usize - 2].end, (" ", ""))
            } else {
                (self.byte_start(body.block.span), ("", " "))
            };
            let what = fname
                .as_deref()
                .map_or("this function".to_string(), |f| format!("`{f}`"));
            self.inserts.push((
                at,
                format!(
                    "{}if not ({cond}) then error({}, 2) end{}",
                    pad.0,
                    luau_string(&format!("{what} is {text} and cannot run here")),
                    pad.1
                ),
            ));
        }

        if function_needs_rewrite(body) {
            self.function_with_header(rest, body);
        } else {
            let children = function_children(body);
            self.stitch(rest, &children, |d, child| match child {
                Child::Expr(e) => d.expr(e),

                Child::Block(b) => d.block(b),

                Child::Function(b) => d.function_block(b),
            });
        }

        let _ = decl_start;
        let mut tail = String::new();

        if let Some(f) = &fname {
            if is_test {
                self.test_names.push((f.clone(), body.is_async.is_some()));

                if !self.options.tests {
                    let std = self.std();
                    tail.push_str(&format!(" {std}.test({}, {f})", luau_string(f)));
                }
            }

            if !user.is_empty() {
                let std = self.std();
                tail.push_str(&format!(" {std}.attach({f}, {{ {} }})", user.join(", ")));
            }

            if exported {
                self.exports.push((f.clone(), f.clone()));
            }
        }

        if !tail.is_empty() {
            let end = self.byte_end(span);
            self.generate(end, &tail);
        }
    }

    // --- macros ----------------------------------------------------------------

    /*
    A macro expands by substitution and a second compile. The body's tokens
    are joined onto one line with each parameter replaced by its argument's
    source text, then parsed and rendered like any Alloy, so intrinsics
    and sugar inside the body work. A body with statements in expression
    position wraps in a closure.
    */
    fn expand_macro(&mut self, m: &MacroRef, args: &[Expr], span: TokSpan) -> String {
        let anchor = self.byte_start(span);
        let arg_texts: Vec<String> = args
            .iter()
            .map(|a| self.text_of(a.span()).to_string())
            .collect();
        let mut subst: HashMap<&str, String> = HashMap::new();

        for (i, p) in m.params.iter().enumerate() {
            let text = arg_texts.get(i).cloned().unwrap_or("nil".to_string());
            subst.insert(p.as_str(), text);
        }

        let rest: Vec<String> = arg_texts.iter().skip(m.params.len()).cloned().collect();

        // Hygiene: a local the body declares gets a name of its own per
        // expansion, so an argument that names the caller's `tmp` never
        // reads the body's `tmp`.
        self.macro_serial += 1;
        let serial = self.macro_serial;
        let mut renames: HashMap<String, String> = HashMap::new();

        for name in body_locals(&m.body) {
            if !m.params.contains(&name) && !name.starts_with("__") {
                renames
                    .entry(name.clone())
                    .or_insert_with(|| format!("{name}__m{serial}"));
            }
        }

        // Substitute whole words in the one-line body.
        let substitute = |text: &str| -> String {
            let mut out = String::new();

            for word in text.split(' ') {
                if !out.is_empty() {
                    out.push(' ');
                }

                if word == "..." && m.variadic {
                    out.push_str(&rest.join(", "));
                } else if let Some(a) = subst.get(word)
                    && word.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    if is_simple_text(a) {
                        out.push_str(a);
                    } else {
                        out.push_str(&format!("({a})"));
                    }
                } else if let Some(r) = renames.get(word) {
                    out.push_str(r);
                } else {
                    out.push_str(word);
                }
            }

            out
        };

        let stmts = substitute(&m.body);
        let tail = m.tail.as_ref().map(|t| substitute(t));

        let source = match (stmts.is_empty(), &tail) {
            (true, Some(t)) => t.clone(),

            (false, None) => stmts.clone(),

            (false, Some(t)) => format!("(function() {stmts} return {t} end)()"),

            (true, None) => "nil".to_string(),
        };

        let as_expr = stmts.is_empty() || tail.is_some();
        let nested_src = if as_expr {
            format!("return {source}")
        } else {
            source
        };

        // The nested compile sees this file's macros, one level down.
        self.compile_fragment(&nested_src, anchor, as_expr)
    }

    /// Compiles a piece of Alloy on its own and splices the Luau in: the
    /// body of a macro with its arguments in place, or the match an
    /// intrinsic builds. The piece sees globals and what it names; it
    /// lands in the calling scope, where the names resolve.
    fn compile_fragment(&mut self, nested_src: &str, anchor: u32, as_expr: bool) -> String {
        let macros: Vec<MacroSource> = self
            .macros
            .iter()
            .map(|(name, r)| MacroSource {
                name: name.clone(),
                params: r.params.clone(),
                variadic: r.variadic,
                body: r.body.clone(),
                tail: r.tail.clone(),
            })
            .collect();

        if self.options.macros.len() > 16 {
            self.diagnostics.push(Diagnostic {
                start: anchor,
                end: anchor,
                message: "macro expansion nests too deeply".to_string(),
            });

            return "nil".to_string();
        }

        match crate::compile_with(
            nested_src,
            &EmitOptions {
                file_name: self.options.file_name.clone(),
                macros,
                ..self.options.clone()
            },
        ) {
            Ok(out) => {
                if out.uses_std {
                    self.uses_std = true;
                }

                for d in out.diagnostics {
                    self.diagnostics.push(Diagnostic {
                        start: anchor,
                        end: anchor,
                        message: format!("in macro expansion: {}", d.message),
                    });
                }

                let text = out.ship.replace('\n', " ");
                let prefix = format!(
                    "local __alloy = require({}) ",
                    luau_string(&self.options.std_require)
                );
                let text = text
                    .strip_prefix(&prefix)
                    .unwrap_or(&text)
                    .trim()
                    .to_string();

                if !as_expr {
                    return text;
                }

                match text.strip_prefix("return ") {
                    Some(value) => value.to_string(),

                    // Statements before the value, a hoisted temp: a
                    // closure keeps them in expression position.
                    None => format!("(function() {text} end)()"),
                }
            }

            Err(e) => {
                self.diagnostics.push(Diagnostic {
                    start: anchor,
                    end: anchor,
                    message: format!("macro expansion failed: {e}"),
                });

                "nil".to_string()
            }
        }
    }
}

/// Text that reads the same with or without parentheses around it.
/// The names a macro body declares: after `local`, the variables of a
/// `for`, and the parameters of a `function` inside it. The body is one
/// line of tokens joined by spaces.
fn body_locals(body: &str) -> Vec<String> {
    let words: Vec<&str> = body.split(' ').collect();
    let is_name = |w: &str| {
        w.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && w.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !matches!(w, "function" | "in" | "do" | "end" | "local" | "for")
    };
    let mut out = Vec::new();
    let mut i = 0;

    while i < words.len() {
        match words[i] {
            "local" => {
                let mut j = i + 1;

                if words.get(j) == Some(&"function") {
                    j += 1;
                }

                while let Some(w) = words.get(j) {
                    if is_name(w) {
                        out.push((*w).to_string());
                        j += 1;

                        // A type annotation runs to the next comma or `=`.
                        if words.get(j) == Some(&":") {
                            while let Some(t) = words.get(j)
                                && !matches!(*t, "," | "=")
                                && !(j > i + 1 && is_name(t) && words.get(j - 1) == Some(&","))
                            {
                                j += 1;
                            }
                        }
                    }

                    if words.get(j) == Some(&",") {
                        j += 1;
                    } else {
                        break;
                    }
                }
            }

            "for" => {
                let mut j = i + 1;

                while let Some(w) = words.get(j)
                    && !matches!(*w, "in" | "=" | "do")
                {
                    if is_name(w) {
                        out.push((*w).to_string());
                    }

                    j += 1;
                }
            }

            "function" => {
                let mut j = i + 1;

                while let Some(w) = words.get(j)
                    && *w != "("
                    && !matches!(*w, "end" | "local")
                {
                    j += 1;
                }

                if words.get(j) == Some(&"(") {
                    let mut depth = 0i32;
                    let mut at_start = true;

                    while let Some(w) = words.get(j) {
                        match *w {
                            "(" | "{" | "[" | "<" => depth += 1,
                            ")" | "}" | "]" | ">" => depth -= 1,
                            _ => {}
                        }

                        if depth == 0 {
                            break;
                        }

                        if at_start && is_name(w) && depth == 1 && *w != "self" {
                            out.push((*w).to_string());
                        }

                        at_start = matches!(*w, "(" | ",") && depth == 1;
                        j += 1;
                    }
                }
            }

            _ => {}
        }

        i += 1;
    }

    out
}

fn is_simple_text(t: &str) -> bool {
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '"' || c == '\'')
}

fn join_tests(tests: &[String]) -> String {
    if tests.is_empty() {
        "true".to_string()
    } else {
        tests.join(" and ")
    }
}

/// Whether a block, or a nested block of it, returns a value. A function
/// literal inside has its own returns and does not count.
fn returns_value(block: &Block) -> bool {
    block.stmts.iter().any(|stmt| match stmt {
        Stmt::Return(r) => !r.values.is_empty(),

        _ => stmt_children(stmt).iter().any(|child| match child {
            Child::Block(b) => returns_value(b),

            _ => false,
        }),
    })
}

/// The type of a literal, for a parameter that takes its value.
fn literal_type(e: &Expr) -> Option<String> {
    match e {
        Expr::Number(_) => Some("number".to_string()),

        Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. } => Some("string".to_string()),

        Expr::True(_) | Expr::False(_) => Some("boolean".to_string()),

        _ => None,
    }
}

fn if_has_local(i: &If) -> bool {
    i.branches
        .iter()
        .any(|(c, _)| matches!(c, Cond::Local { .. }))
}

/// The names a pattern binds, in order.
fn pattern_binds(p: &Pattern) -> Vec<TokSpan> {
    match p {
        Pattern::Wildcard(_) | Pattern::Literal(_) | Pattern::Path(_) => Vec::new(),

        Pattern::Bind(n) => vec![*n],

        Pattern::Variant { args, .. } => args.iter().flat_map(pattern_binds).collect(),

        Pattern::Struct { fields, .. } => fields
            .iter()
            .flat_map(|f| match &f.pattern {
                Some(p) => pattern_binds(p),

                None => vec![f.field],
            })
            .collect(),

        Pattern::Array { items, rest, .. } => {
            let mut v: Vec<TokSpan> = items.iter().flat_map(pattern_binds).collect();

            if let Some(r) = rest {
                v.push(*r);
            }

            v
        }

        Pattern::Or(a, _, _) => pattern_binds(a),
    }
}

impl<'s> Desugar<'s> {
    // --- spans -------------------------------------------------------------

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

    /// The Luau of a `@cfg` condition: names the runtime reads, joined
    /// by `not`, `and`, `or`, or by `any(...)` and `all(...)`.
    fn cfg_condition(&mut self, args: &[Expr]) -> Result<String, String> {
        let [arg] = args else {
            return Err("`@cfg` takes one condition: `@cfg(server)`, `@cfg(not client)`, `@cfg(server and studio)`".to_string());
        };
        let std = self.std();

        self.cfg_expr(arg, std)
    }

    fn cfg_expr(&self, e: &Expr, std: &str) -> Result<String, String> {
        const FLAGS: &[&str] = &["server", "client", "studio", "edit", "running", "test"];

        match e {
            Expr::Name(t) => {
                let name = self.text_of(*t);

                if FLAGS.contains(&name) {
                    Ok(format!("{std}.cfg.{name}()"))
                } else {
                    Err(format!(
                        "`@cfg({name})`: the conditions are {}",
                        FLAGS.join(", ")
                    ))
                }
            }

            Expr::Paren { inner, .. } => Ok(format!("({})", self.cfg_expr(inner, std)?)),

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "not" => {
                Ok(format!("not {}", self.cfg_expr(operand, std)?))
            }

            Expr::Binary { op, lhs, rhs, .. } if matches!(self.text_of(*op), "and" | "or") => {
                Ok(format!(
                    "({} {} {})",
                    self.cfg_expr(lhs, std)?,
                    self.text_of(*op),
                    self.cfg_expr(rhs, std)?
                ))
            }

            Expr::Call {
                func,
                method: None,
                args: CallArgs::Paren(args),
                ..
            } if matches!(&**func, Expr::Name(f) if matches!(self.text_of(*f), "any" | "all")) => {
                let Expr::Name(f) = &**func else {
                    unreachable!()
                };
                let joiner = if self.text_of(*f) == "any" { " or " } else { " and " };
                let parts = args
                    .iter()
                    .map(|a| self.cfg_expr(a, std))
                    .collect::<Result<Vec<_>, _>>()?;

                if parts.is_empty() {
                    return Err(format!("`@cfg({}())` names no condition", self.text_of(*f)));
                }

                Ok(format!("({})", parts.join(joiner)))
            }

            _ => Err("`@cfg` takes names joined by `not`, `and`, `or`, `any(...)`, or `all(...)`: `@cfg(server and not studio)`".to_string()),
        }
    }

    fn diagnose(&mut self, span: TokSpan, message: &str) {
        self.diagnostics.push(Diagnostic {
            start: self.byte_start(span),
            end: self.byte_end(span),
            message: message.to_string(),
        });
    }

    /// The std table, marking the file as needing the require.
    fn std(&mut self) -> &'static str {
        self.uses_std = true;

        "__alloy"
    }

    /// The prefix for an ambient type: `__alloy.` in code, nothing in a
    /// definitions file, where the std types are globals.
    fn type_std(&mut self) -> &'static str {
        if self.options.definitions {
            ""
        } else {
            self.uses_std = true;

            "__alloy."
        }
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

    /*
    A mapped type compiles to an inline type function call over the
    source's properties. Luau's `index` and `keyof` type functions cannot
    rebuild a table, so the loop runs inside a user-defined type function
    the alias declares once, named after the shape.
    */
    fn mapped_type(
        &mut self,
        key: TokSpan,
        source: TokSpan,
        modifier: Option<TokSpan>,
        optional: bool,
    ) -> String {
        let _ = key;
        let src = self.text_of(source).to_string();
        let kind = match (modifier.map(|m| self.text_of(m)), optional) {
            (Some("read"), _) => "read",

            (Some("write"), _) => "write",

            (None, true) => "optional",

            _ => "same",
        };

        if !self.mapped_used.contains(&kind) {
            self.mapped_used.push(kind);
        }

        format!("__mapped_{kind}<{src}>")
    }

    /// The type function behind one mapped shape, on one line.
    fn mapped_type_function(kind: &str) -> String {
        // A definitions file is checked in strict mode, so every local
        // carries a type.
        let value = match kind {
            "optional" => "types.unionof((v.read or v.write) :: any, types.singleton(nil))",

            _ => "(v.read or v.write) :: any",
        };
        let entry = match kind {
            "read" => "{ read = value }",

            "write" => "{ write = value }",

            _ => "{ read = value, write = value }",
        };

        format!(
            "type function __mapped_{kind}(T) local function collect(t): {{ [any]: any }} if t:is(\"intersection\") then local all = {{}} for _, c in t:components() do for k, v in collect(c) do all[k] = v end end return all end return t:properties() end local props: {{ [any]: any }} = {{}} for k, v in collect(T) do local value: any = {value} props[k] = {entry} end return types.newtable(props) end"
        )
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

    // --- blocks and statements --------------------------------------------

    fn block(&mut self, block: &Block) {
        if block.span.is_empty() {
            return;
        }

        self.declared.push(Vec::new());
        self.scopes.push(HashSet::new());
        let mut cursor = self.byte_start(block.span);

        for stmt in &block.stmts {
            let start = self.byte_start(stmt.span());
            self.copy(cursor, start);
            let before = self.r.out_len();
            self.stmt(stmt);
            self.keep_lines(stmt.span(), before);
            cursor = self.byte_end(stmt.span());
        }

        self.copy(cursor, self.byte_end(block.span));
        self.scopes.pop();
        self.declared.pop();
    }

    /// A statement's output holds every newline of its source span, so
    /// the lines after it stay where they are. A replacement written as
    /// one line over several source lines, `local x = if local ... then
    /// ... else ...`, gets the missing newlines copied from the end of
    /// the span.
    fn keep_lines(&mut self, span: TokSpan, before: u32) {
        let start = self.byte_start(span) as usize;
        // Trailing whitespace of the span copies after the statement.
        let end = start
            + self.src[start..self.byte_end(span) as usize]
                .trim_end()
                .len();
        let want = self.src[start..end].matches('\n').count();
        let have = self.r.newlines_since(before);

        if have >= want {
            return;
        }

        let positions: Vec<u32> = self.src[start..end]
            .match_indices('\n')
            .map(|(i, _)| (start + i) as u32)
            .collect();

        for nl in &positions[positions.len() - (want - have)..] {
            self.r.copy(*nl, nl + 1);
        }
    }

    /// Renders a function body: its temps start fresh behind a barrier, and
    /// its parameters are in scope.
    fn function_block(&mut self, body: &FunctionBody) {
        let saved = self.barrier;
        self.barrier = self.declared.len();
        self.scopes.push(HashSet::new());
        self.declare_params(body);
        self.ret_types
            .push(body.ret_type.map(|t| self.text_of(t).trim().to_string()));
        self.block(&body.block);
        self.ret_types.pop();
        self.scopes.pop();
        self.barrier = saved;
    }

    /// Reports if the function under render returns a `Result`, which is
    /// what `try` needs.
    fn in_result_function(&self) -> bool {
        let Some(Some(ty)) = self.ret_types.last() else {
            return false;
        };
        let head = ty.split(['<', '?']).next().unwrap_or(ty).trim();

        head == "Result" || self.result_aliases.contains(head)
    }

    fn stmt(&mut self, stmt: &Stmt) {
        // Declarations come first so a later statement sees them.
        match stmt {
            Stmt::Local(l) => {
                for b in &l.names {
                    self.declare_binding(b);
                }
            }

            Stmt::LocalFunction(f) => self.declare_name(f.name),

            Stmt::Function(f) if f.path.len() == 1 && f.exported => self.declare_name(f.path[0]),

            Stmt::Enum(e) => {
                self.declare_name(e.name);
                let variants = e
                    .variants
                    .iter()
                    .map(|v| (self.text_of(v.name).to_string(), v.payload.len()))
                    .collect();
                self.enums
                    .insert(self.text_of(e.name).to_string(), variants);
            }

            Stmt::Import(i) => match &i.kind {
                ImportKind::Namespace(n) => self.declare_name(*n),

                ImportKind::Both(n, specs) => {
                    self.declare_name(*n);

                    for sp in specs {
                        self.declare_name(sp.alias.unwrap_or(sp.name));
                    }
                }

                ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                    for sp in specs {
                        self.declare_name(sp.alias.unwrap_or(sp.name));
                    }
                }
            },

            Stmt::PatternLocal(p) => {
                let names = pattern_binds(&p.pattern);

                for n in names {
                    self.declare_name(n);
                }
            }

            Stmt::Struct(st) => {
                self.declare_name(st.name);
                self.note_struct(st);
            }

            Stmt::Trait(t) => {
                self.declare_name(t.name);
                self.not_constructible
                    .insert(self.text_of(t.name).to_string(), "trait");
            }

            Stmt::Remote(r) => {
                self.declare_name(r.name);
                self.not_constructible
                    .insert(self.text_of(r.name).to_string(), "remote");
            }

            Stmt::Attribute(a) => {
                self.declare_name(a.name);
                self.not_constructible
                    .insert(self.text_of(a.name).to_string(), "attribute");
            }

            Stmt::Interface(i) => {
                let name = self.text_of(i.name).to_string();
                self.not_constructible.insert(name.clone(), "interface");
                // An interface with no base has its fields here; one
                // with a base keeps the type function, which sees them.
                if i.extends.is_empty() {
                    self.note_field_types(&name, &i.fields);
                }
            }

            _ => {}
        }

        if !self.stmt_needs_desugar(stmt) {
            self.copy_span(stmt.span());

            return;
        }

        // Render the statement into a side buffer first, so the hoists it
        // asks for can go in front of it on the same line.
        let saved_hoists = std::mem::take(&mut self.hoists);
        let saved_next = self.temp_next;
        self.temp_next = 0;

        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        self.stmt_inner(stmt);
        std::mem::swap(&mut self.r, &mut side);

        let hoists = std::mem::replace(&mut self.hoists, saved_hoists);
        self.temp_next = saved_next;

        for h in hoists {
            match h {
                Hoist::Temp {
                    index,
                    value,
                    anchor,
                } => {
                    let keyword = if self.temp_declared(index) {
                        ""
                    } else {
                        self.declare_temp(index);

                        "local "
                    };

                    match value {
                        HoistValue::Text(text) => {
                            let line = format!("{keyword}_{index} = {text} ");
                            self.generate(anchor, &line);
                        }

                        // The value keeps its source chunks, so a function
                        // literal inside it keeps its lines.
                        HoistValue::Rendered(rendered) => {
                            self.generate(anchor, &format!("{keyword}_{index} = "));
                            self.r.append(rendered);
                            self.generate(anchor, " ");
                        }
                    }
                }

                Hoist::Stmt { text, anchor } => {
                    let line = format!("{text} ");
                    self.generate(anchor, &line);
                }

                Hoist::Fresh {
                    name,
                    value,
                    anchor,
                } => {
                    let line = format!("local {name} = {value} ");
                    self.generate(anchor, &line);
                }
            }
        }

        self.r.append(side);
    }

    /// Reports if a statement needs rewriting. The tree check is exact for
    /// nodes; ambient names and word operators need the source text.
    fn stmt_needs_desugar(&self, s: &Stmt) -> bool {
        if stmt_needs_desugar(s) {
            return true;
        }

        let text = self.text_of(s.span());

        match s {
            Stmt::Local(l) if !l.attrs.is_empty() => return true,

            Stmt::Function(f) if self.params_have_attrs(&f.body) => return true,

            Stmt::LocalFunction(f) if self.params_have_attrs(&f.body) => return true,

            _ => {}
        }

        AMBIENT.iter().any(|n| text.contains(n))
            || text.contains("import(")
            || text.contains("import<<")
            || self.structs.iter().any(|name| struct_called(text, name))
            || self.structs.iter().any(|name| struct_braced(text, name))
            || WORD_OPS.iter().any(|w| text.contains(w))
            || self.ext_methods.iter().any(|m| text.contains(m.as_str()))
            || self
                .ext_statics
                .values()
                .flatten()
                .any(|m| text.contains(m.as_str()))
    }

    fn stmt_inner(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Struct(st) => self.struct_decl(st),

            Stmt::Trait(t) => self.trait_decl(t),

            Stmt::Interface(i) => self.interface_decl(i),

            Stmt::Remote(r) => self.remote_decl(r),

            Stmt::Attribute(a) => self.attribute_decl(a),

            Stmt::Macro(_) => {
                // The declaration exists at compile time only; its lines
                // stay blank.
                let span = stmt.span();
                self.blank_lines(self.byte_start(span), self.byte_end(span));
            }

            Stmt::Function(f) if !f.attrs.is_empty() => self.attributed_function(
                stmt.span(),
                &f.attrs,
                &f.body,
                f.path.first().copied(),
                f.exported,
                false,
            ),

            Stmt::LocalFunction(f) if !f.attrs.is_empty() => self.attributed_function(
                stmt.span(),
                &f.attrs,
                &f.body,
                Some(f.name),
                f.exported,
                true,
            ),

            Stmt::Import(i) => self.import_stmt(i),

            Stmt::ExportList(e) => self.export_list(e),

            Stmt::ExportDefault { value, span } => {
                let anchor = self.byte_start(*span);
                self.has_default_export = true;
                self.generate(anchor, "return ");
                self.expr(value);
            }

            Stmt::Enum(e) => self.enum_decl(e),

            Stmt::Impl(i) => self.impl_decl(i),

            Stmt::Match(m) => self.match_stmt(m),

            Stmt::PatternLocal(p) => self.pattern_local(p),

            Stmt::If(i) if if_has_local(i) => self.if_with_locals(stmt.span(), i),

            Stmt::While(w) if matches!(w.cond, Cond::Local { .. }) => {
                self.while_with_local(stmt.span(), w);
            }

            Stmt::Local(l) if l.exported => self.exported_local(stmt.span(), l),

            Stmt::Function(f) if f.exported => {
                let anchor = self.byte_start(stmt.span());
                let name = self.text_of(f.path[0]).to_string();
                self.exports.push((name.clone(), name));
                // `export function f` becomes `local function f`.
                let fn_tok = self.toks[stmt.span().start as usize + 1];
                self.generate(anchor, "local ");
                let rest = TokSpan::new(stmt.span().start as usize + 1, stmt.span().end as usize);
                let _ = fn_tok;

                if function_needs_rewrite(&f.body) {
                    self.function_with_header(rest, &f.body);
                } else {
                    let children = function_children(&f.body);
                    self.stitch(rest, &children, |d, child| match child {
                        Child::Expr(e) => d.expr(e),

                        Child::Block(b) => d.block(b),

                        Child::Function(b) => d.function_block(b),
                    });
                }
            }

            Stmt::LocalFunction(f) if f.exported => {
                let name = self.text_of(f.name).to_string();
                self.exports.push((name.clone(), name));
                let rest = TokSpan::new(stmt.span().start as usize + 1, stmt.span().end as usize);

                if function_needs_rewrite(&f.body) {
                    self.function_with_header(rest, &f.body);
                } else {
                    let children = function_children(&f.body);
                    self.stitch(rest, &children, |d, child| match child {
                        Child::Expr(e) => d.expr(e),

                        Child::Block(b) => d.block(b),

                        Child::Function(b) => d.function_block(b),
                    });
                }
            }

            Stmt::Assign(a) if self.assign_needs_rewrite(a) => self.assign(a),

            Stmt::Call(e, span) if chain_has_alloy(e) || self.chain_has_ext(e) => {
                let anchor = self.byte_start(*span);
                let text = self.chain_stmt(e);
                self.generate(anchor, &text);
            }

            // `new X(...) { fields }` alone: the instance lands in a temp and
            // each field assigns through it, as the local form does.
            Stmt::Call(
                Expr::New {
                    name,
                    type_args,
                    args,
                    init: Some(init),
                    span: whole,
                },
                span,
            ) if !self.fields_form(name, args.as_ref(), Some(init)) => {
                let anchor = self.byte_start(*span);
                // A fresh local each time: a reused temp would carry
                // the type of an earlier statement into the checker.
                self.new_stmt_next += 1;
                let binding = format!("_n{}", self.new_stmt_next);
                self.new_init(
                    &format!("local {binding}"),
                    &binding,
                    name,
                    *type_args,
                    args.as_ref(),
                    init,
                    *whole,
                    anchor,
                );
            }

            // `new X(...)`, `await f()`: a call once rendered. `try f()`
            // drops its value into a throwaway local, since the unwrapped
            // value is a name and a name is no statement.
            Stmt::Call(e @ (Expr::New { .. } | Expr::Await { .. } | Expr::Try { .. }), span) => {
                let anchor = self.byte_start(*span);

                if matches!(e, Expr::Try { .. }) {
                    self.generate(anchor, "local _ = ");
                }

                self.expr(e);
            }

            // `@cfg` on a local guards its value; any other attribute
            // has nothing to attach to: a diagnostic, and the text goes
            // so the output stays Luau.
            Stmt::Local(l) if !l.attrs.is_empty() => {
                let mut cfg = None;

                for a in &l.attrs {
                    let name = a.name.map(|n| self.text_of(n)).unwrap_or("").to_string();

                    if name == "cfg" {
                        match self.cfg_condition(&a.args) {
                            Ok(cond) => cfg = Some(cond),

                            Err(message) => self.diagnose(a.span, &message),
                        }
                    }

                    // The target check runs in `check_attrs`, over every
                    // declaration at once.
                    self.blank_lines(self.byte_start(a.span), self.byte_end(a.span));
                }

                // The copy starts where the last attribute ends, so the
                // newline between it and the keyword survives.
                let after_attrs = l
                    .attrs
                    .iter()
                    .map(|a| self.byte_end(a.span))
                    .max()
                    .unwrap_or(0);

                // The value is read only when the condition holds; the
                // binding keeps the value's type, so the code that uses
                // it reads as before. `typeof` sees the value, it does
                // not run it.
                if let Some(cond) = cfg {
                    let plain = l.names.len() == 1
                        && l.values.len() == 1
                        && !self.text_of(l.names[0].name).starts_with(['{', '[']);

                    if plain {
                        let value = &l.values[0];
                        let vs = self.byte_start(value.span());
                        let ve = self.byte_end(value.span());
                        // The copy in `typeof` sits on one line: each
                        // line of the value loses its indent.
                        let shape = self
                            .render_to_string(value)
                            .lines()
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .join(" ");
                        self.copy(after_attrs, vs);
                        self.generate(vs, &format!("(if {cond} then "));
                        self.expr(value);
                        self.generate(ve, &format!(" else nil) :: typeof({shape})"));
                        self.copy(ve, self.byte_end(l.span));

                        return;
                    }

                    self.diagnose(l.span, "`@cfg` goes on a local with one name and one value");
                }

                if local_needs_rewrite(l) {
                    self.local_stmt(l);
                } else {
                    self.copy(after_attrs, self.byte_end(l.span));
                }
            }

            Stmt::Function(f) if self.params_have_attrs(&f.body) => {
                self.check_param_attrs(&f.body);
                self.copy_span(stmt.span());
            }

            Stmt::LocalFunction(f) if self.params_have_attrs(&f.body) => {
                self.check_param_attrs(&f.body);
                self.copy_span(stmt.span());
            }

            Stmt::Local(l) if local_needs_rewrite(l) => self.local_stmt(l),

            // `local m: HashMap<K, V> = HashMap.new()`: the call takes the
            // annotation's arguments, since the solver infers none.
            Stmt::Local(l) if self.annotated_constructor(l).is_some() => {
                self.expected_generic = self.annotated_constructor(l);
                let span = stmt.span();
                let children = stmt_children(stmt);
                self.stitch(span, &children, |d, child| match child {
                    Child::Expr(e) => d.expr(e),

                    Child::Block(b) => d.block(b),

                    Child::Function(b) => d.function_block(b),
                });
                self.expected_generic = None;
            }

            Stmt::Delete { expr, span } => {
                let anchor = self.byte_start(*span);
                let std = self.std();
                // The target renders in place, so the editor maps a
                // position inside it.
                self.generate(anchor, &format!("{std}.delete("));
                self.expr(expr);
                let close = self.byte_end(*span);
                let mut tail = ")".to_string();

                // A field or an index empties after its value is gone, so
                // the table holds nothing destroyed. The check artifact
                // writes through `any`: a `read` field takes no assignment.
                if let Expr::Index {
                    object,
                    key,
                    optional: false,
                    ..
                } = expr
                {
                    let obj = self.render_to_string(object);
                    let slot = match key {
                        IndexKey::Field(n) => format!(".{}", self.text_of(*n)),

                        IndexKey::Computed(e) => format!("[{}]", self.render_to_string(e)),
                    };
                    // A `;` keeps `f(x) (y).z = nil` from reading as a call.
                    let receiver = if self.options.check {
                        format!("; ({obj} :: any)")
                    } else {
                        format!(" {obj}")
                    };
                    tail.push_str(&format!("{receiver}{slot} = nil"));
                }

                self.generate(close, &tail);
            }

            Stmt::Function(f) if function_needs_rewrite(&f.body) => {
                self.function_with_header(stmt.span(), &f.body);
            }

            Stmt::LocalFunction(f) if function_needs_rewrite(&f.body) => {
                self.function_with_header(stmt.span(), &f.body);
            }

            // `class` parses for the classes RFC and has no lowering yet:
            // a diagnostic, and the text goes, so the output stays Luau.
            Stmt::Class(c) => {
                self.diagnose(
                    c.span,
                    "`class` is parsed and not compiled yet; a `struct` with an `impl` is the form that runs",
                );
                self.blank_lines(self.byte_start(c.span), self.byte_end(c.span));
            }

            Stmt::GenericFor(f) if for_needs_rewrite(f) => self.generic_for(stmt.span(), f),

            _ => {
                let span = stmt.span();
                let children = stmt_children(stmt);
                let reevaluated = reevaluated_conditions(stmt);
                let (narrow_blocks, narrow_after) = match stmt {
                    Stmt::If(i) => self.narrowings(i),

                    _ => (Vec::new(), None),
                };
                self.stitch(span, &children, |d, child| match child {
                    Child::Expr(e) => {
                        let guard = reevaluated.contains(&std::ptr::from_ref::<Expr>(e));

                        if guard {
                            d.no_hoist += 1;
                        }

                        d.expr(e);

                        if guard {
                            d.no_hoist -= 1;
                        }
                    }

                    Child::Block(b) => {
                        if let Some((_, prefix)) =
                            narrow_blocks.iter().find(|(at, _)| *at == b.span.start)
                        {
                            let anchor = d.byte_start(b.span);
                            d.generate(anchor, prefix);
                        }

                        d.block(b);
                    }

                    Child::Function(b) => d.function_block(b),
                });

                if let Some(text) = narrow_after {
                    let anchor = self.byte_end(span);
                    self.generate(anchor, &text);
                }
            }
        }
    }

    /*
    Renders a parent span by copying the text between its children and
    rendering each child. The children come in source order. This is the
    one routine that lets every node kind survive a desugar in one of its
    descendants without a renderer of its own.
    */
    fn stitch<F>(&mut self, span: TokSpan, children: &[Child<'_>], mut render: F)
    where
        F: FnMut(&mut Self, &Child<'_>),
    {
        let mut cursor = self.byte_start(span);
        let end = self.byte_end(span);

        for child in children {
            let cspan = child.span();

            if cspan.is_empty() {
                continue;
            }

            let cs = self.byte_start(cspan);
            self.copy(cursor, cs);
            render(self, child);
            cursor = self.byte_end(cspan);
        }

        self.copy(cursor, end);
    }

    /// Stitches children between two byte offsets, copying the text around them.
    fn stitch_between(&mut self, start: u32, end: u32, children: &[Child<'_>]) {
        let mut cursor = start;

        for child in children {
            let cspan = child.span();

            if cspan.is_empty() {
                continue;
            }

            let cs = self.byte_start(cspan);
            self.copy(cursor, cs);

            match child {
                Child::Expr(e) => self.expr(e),

                Child::Block(b) => self.block(b),

                Child::Function(b) => self.function_block(b),
            }

            cursor = self.byte_end(cspan);
        }

        self.copy(cursor, end);
    }

    // --- temps -------------------------------------------------------------

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

    fn expr(&mut self, e: &Expr) {
        let anchor = self.byte_start(e.span());

        match e {
            Expr::Name(span) => {
                let name = self.text_of(*span);

                if let Some(path) = self.renamed(name) {
                    self.generate(anchor, &path);
                } else if AMBIENT.contains(&name) && !self.is_local(name) {
                    let std = self.std();
                    self.generate(anchor, &format!("{std}.{name}"));
                } else {
                    self.copy_span(*span);
                }
            }

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "bnot" => {
                let inner = self.render_to_string(operand);
                self.generate(anchor, &format!("bit32.bnot({inner})"));
            }

            Expr::Binary { op, lhs, rhs, span } if self.is_coalesce(*op) => {
                self.coalesce(*span, lhs, rhs);
            }

            Expr::Binary { op, lhs, rhs, .. } if self.word_binop(*op).is_some() => {
                let (kind, name) = self.word_binop(*op).unwrap();
                let l = self.render_to_string(lhs);
                let r = self.render_to_string(rhs);
                let text = match kind {
                    WordOp::Bit => format!("bit32.{name}({l}, {r})"),

                    WordOp::In => {
                        let std = self.std();

                        format!("{std}.contains({r}, {l})")
                    }
                };
                self.generate(anchor, &text);
            }

            // `Signal.new<<A, B>>()`: the std types the statics with a
            // type pack, which takes one parenthesized argument.
            Expr::Call {
                func,
                method: None,
                type_args: Some(t),
                args,
                ..
            } if self.is_signal_new(func) => {
                let std = self.std();
                let targs = pack_type_args(&type_args_text(self.text_of(*t)));
                let a = self.args_text(args);
                self.generate(anchor, &format!("{std}.Signal.new{targs}{a}"));
            }

            Expr::Index { .. } | Expr::Call { .. } | Expr::Child { .. } | Expr::NonNil { .. }
                if chain_has_alloy(e)
                    || self.chain_has_ext(e)
                    || self.is_struct_call(e)
                    || self.is_import_call(e)
                    || self.expected_generic.is_some() =>
            {
                let text = self.chain_expr(e);
                self.generate(anchor, &text);
            }

            Expr::Ternary {
                cond,
                then_value,
                else_value,
                ..
            } => {
                let c = self.render_to_string(cond);
                let a = self.render_to_string(then_value);
                let b = self.render_to_string(else_value);
                self.generate(anchor, &format!("(if {c} then {a} else {b})"));
            }

            Expr::Is {
                expr,
                negated,
                name,
                ..
            } => {
                let text = self.is_test(expr, *name, *negated);
                self.generate(anchor, &text);
            }

            Expr::Satisfies { expr, ty, .. } => {
                let inner = self.render_to_string(expr);
                let ty = self.text_of(*ty).to_string();
                self.generate(anchor, &format!("({inner} :: {ty})"));
            }

            Expr::Array { items, span } => {
                // An empty literal has no element type; the check artifact
                // lets the annotation on the left decide it.
                let cast = self.options.check && items.is_empty();
                let std = self.std();
                let open_text = if cast {
                    format!("({std}.Array.from({{")
                } else {
                    format!("{std}.Array.from({{")
                };
                self.generate(anchor, &open_text);
                let open = self.toks[span.start as usize].end;
                let close = self.toks[span.end as usize - 1].start;
                let children: Vec<Child<'_>> = items.iter().map(Child::Expr).collect();
                self.stitch_between(open, close, &children);
                self.generate(close, if cast { "}) :: any)" } else { "})" });
            }

            Expr::Table { fields, span }
                if fields.iter().any(|f| matches!(f, TableField::Spread(_))) =>
            {
                self.spread_table(*span, fields);
            }

            Expr::MethodRef { object, name, .. } => {
                let obj = self.reusable(object);
                let method = self.text_of(*name).to_string();
                let std = self.std();
                let bound = self.any_cast(&format!("{std}.bind({obj}, {obj}.{method})"));
                self.generate(anchor, &bound);
            }

            Expr::New {
                name,
                type_args,
                args,
                init,
                span,
            } => {
                // A fields table copies as it is, since it may span
                // lines and generated text never holds a newline.
                if self.fields_form(name, args.as_ref(), init.as_deref())
                    && let Some(table) = init.as_deref()
                {
                    self.check_new(name, args.as_ref(), Some(table), *span);
                    let n = self.render_to_string(name);
                    // Inside the struct's own impl the instance carries the
                    // full view, so `self.count` in `new` type checks.
                    let full_view = self.impl_target.as_deref() == Some(n.as_str())
                        && self.has_private_view(&n);
                    let open = if full_view {
                        format!("(({}(", self.raw_ctor(&n))
                    } else {
                        format!("{}(", self.raw_ctor(&n))
                    };
                    self.generate(anchor, &open);
                    self.expr(table);
                    let close = if full_view {
                        format!(") :: any) :: {n}__all)")
                    } else {
                        ")".to_string()
                    };
                    self.generate(self.byte_end(table.span()), &close);
                } else {
                    let head =
                        self.new_head(name, *type_args, args.as_ref(), init.as_deref(), *span);
                    self.generate(anchor, &head);

                    if let Some(table) = init.as_deref() {
                        self.expr(table);
                        self.generate(self.byte_end(table.span()), ")");
                    }
                }
            }

            Expr::Await { operand, .. } => {
                let inner = self.render_to_string(operand);
                let std = self.std();
                self.generate(anchor, &format!("{std}.await({inner})"));
            }

            Expr::Try { operand, span } => {
                let text = self.try_expr(operand, *span);
                self.generate(anchor, &text);
            }

            Expr::AsyncBlock { block, span } | Expr::TryBlock { block, span } => {
                let helper = if matches!(e, Expr::AsyncBlock { .. }) {
                    "future"
                } else {
                    "try_block"
                };
                let std = self.std();
                self.generate(anchor, &format!("{std}.{helper}(function()"));
                // The two keywords are replaced; the block and `end` copy.
                let after_keywords = self.toks[span.start as usize + 1].end;
                let end_tok = self.toks[span.end as usize - 1];
                let body_start = self.block_start_or(block, end_tok.start);
                self.copy(after_keywords, body_start);
                self.block(block);
                let after_block = self.block_end_or(block, body_start);
                self.copy(after_block, end_tok.start);
                self.copy(end_tok.start, end_tok.end);
                self.generate(end_tok.end, ")");
            }

            Expr::Macro { name, args, span } => {
                let mname = self.text_of(*name).to_string();

                if let Some(m) = self.macros.get(&mname).cloned() {
                    let text = self.expand_macro(&m, args, *span);
                    self.generate(anchor, &text);
                } else {
                    let text = self.intrinsic(*name, args, *span);
                    self.generate(anchor, &text);
                }
            }

            Expr::Match(m) => self.match_expr(m),

            Expr::IfElse {
                branches,
                else_value,
                span,
            } if branches
                .iter()
                .any(|(c, _)| matches!(c, Cond::Local { .. })) =>
            {
                self.if_expr_with_locals(*span, branches, else_value);
            }

            Expr::Function { body, .. } if function_needs_rewrite(body) => {
                self.function_with_header(e.span(), body);
            }

            _ => {
                let children = expr_children(e);

                if children.is_empty() {
                    self.copy_span(e.span());
                } else {
                    self.stitch(e.span(), &children, |d, child| match child {
                        Child::Expr(e) => d.expr(e),

                        Child::Block(b) => d.block(b),

                        Child::Function(b) => d.function_block(b),
                    });
                }
            }
        }
    }

    fn block_start_or(&self, block: &Block, default: u32) -> u32 {
        if block.span.is_empty() {
            default
        } else {
            self.byte_start(block.span)
        }
    }

    fn block_end_or(&self, block: &Block, default: u32) -> u32 {
        if block.span.is_empty() {
            default
        } else {
            self.byte_end(block.span)
        }
    }

    /// `Signal.new` on the std `Signal`, not on a local of that name.
    fn is_signal_new(&self, func: &Expr) -> bool {
        let Expr::Index {
            object,
            key: IndexKey::Field(f),
            ..
        } = func
        else {
            return false;
        };

        matches!(object.as_ref(), Expr::Name(n)
            if self.text_of(*n) == "Signal" && !self.is_local("Signal"))
            && self.text_of(*f) == "new"
    }

    fn is_coalesce(&self, op: TokSpan) -> bool {
        op.end - op.start == 2 && self.text_of(op) == "??"
    }

    fn is_coalesce_assign(&self, op: TokSpan) -> bool {
        op.end - op.start == 3 && self.text_of(op) == "??="
    }

    fn word_binop(&self, op: TokSpan) -> Option<(WordOp, &'static str)> {
        if op.end - op.start != 1 {
            return None;
        }

        match self.text_of(op) {
            "band" => Some((WordOp::Bit, "band")),

            "bor" => Some((WordOp::Bit, "bor")),

            "bxor" => Some((WordOp::Bit, "bxor")),

            "shl" => Some((WordOp::Bit, "lshift")),

            "shr" => Some((WordOp::Bit, "rshift")),

            "in" => Some((WordOp::In, "contains")),

            _ => None,
        }
    }

    /*
    `a ?? b` renders as `(if A == nil then B else A)`.

    A simple left side reads twice in place. Any other left side hoists into
    a temp so it evaluates once. The right side renders inline either way:
    it evaluates only when the left is nil, which is the point.
    */
    fn coalesce(&mut self, span: TokSpan, lhs: &Expr, rhs: &Expr) {
        let anchor = self.byte_start(span);
        let left = self.reusable(lhs);
        let right = self.render_to_string(rhs);
        self.generate(
            anchor,
            &format!("(if {left} == nil then {right} else {left})"),
        );
    }

    /*
    `x is T` by the name on the right. A primitive tests `type`, a Roblox
    datatype tests `typeof`, an Instance class tests `IsA`, an enum tests
    the `EnumType`, and any other name is an Alloy struct's metatable.
    */
    fn is_test(&mut self, expr: &Expr, name: TokSpan, negated: bool) -> String {
        let x = self.reusable(expr);
        let n = self.text_of(name).to_string();

        if n == "nil" {
            return if negated {
                format!("({x} ~= nil)")
            } else {
                format!("({x} == nil)")
            };
        }

        let test = if PRIMITIVES.contains(&n.as_str()) {
            format!("type({x}) == \"{n}\"")
        } else if let Some(item) = n.strip_prefix("Enum.") {
            format!("typeof({x}) == \"EnumItem\" and {x}.EnumType == Enum.{item}")
        } else if INSTANCE_CLASSES.contains(&n.as_str()) {
            if n == "Instance" {
                // The root class: `typeof` alone answers, and an `IsA`
                // on a value typed `any` trips the solver.
                format!("typeof({x}) == \"Instance\"")
            } else {
                format!("typeof({x}) == \"Instance\" and {x}:IsA(\"{n}\")")
            }
        } else if DATATYPES.contains(&n.as_str()) {
            format!("typeof({x}) == \"{n}\"")
        } else if self.enums.contains_key(&n) {
            format!("{n}.is({x})")
        } else {
            format!("getmetatable({}) == {n}", self.any_cast(&x))
        };

        if negated {
            format!("(not ({test}))")
        } else {
            format!("({test})")
        }
    }

    /// `try expr`: hoist the Result, return it on Err, yield the payload.
    fn try_expr(&mut self, operand: &Expr, span: TokSpan) -> String {
        let anchor = self.byte_start(span);

        // At the top level `return` leaves the module, which the emit
        // still writes; inside a function the return type must take an
        // Err.
        if !self.ret_types.is_empty() && !self.in_result_function() {
            self.diagnose(
                span,
                "`try` works only inside a function that returns Result; it returns the Err from that function",
            );
        }

        let value = match operand {
            Expr::Await { operand: inner, .. } => {
                let x = self.render_to_string(inner);
                let std = self.std();
                // A call to an async function declared to return a Result
                // settles with the Result: the typed form says so.
                let known = matches!(&**inner, Expr::Call { func, method: None, .. }
                    if matches!(&**func, Expr::Name(n) if self.result_asyncs.contains(self.text_of(*n))));
                let helper = if known {
                    "try_await_result"
                } else {
                    "try_await"
                };

                format!("{std}.{helper}({x})")
            }

            other => self.render_to_string(other),
        };
        let temp = self.hoist_text(value, anchor);
        let returned = self.any_cast(&temp);
        self.hoist_stmt(
            format!("if {temp}.tag == \"Err\" then return {returned} end"),
            anchor,
        );

        // `_1` is `T | E` to the checker; `unwrap` is `T`. The ship
        // artifact reads the field, since the tag was just checked.
        if self.options.check {
            format!("{temp}:unwrap()")
        } else {
            format!("{temp}._1")
        }
    }

    /// The raw constructor the fields form calls: the class table itself
    /// through `__call`, or in the check artifact its typed `__new`.
    fn raw_ctor(&self, name: &str) -> String {
        if self.options.check {
            format!("{name}.__new")
        } else {
            name.to_string()
        }
    }

    /// A test text with every read from `path` cast to `any` in the check
    /// artifact: across an `or`, the checker refines each side by its own
    /// tag and cannot read the other side's fields.
    fn cast_tests(&self, test: &str, path: &str) -> String {
        if !self.options.check {
            return test.to_string();
        }

        test.replace(&format!("{path}."), &format!("({path} :: any)."))
    }

    /// An access path with its root cast to `any` in the check artifact:
    /// `_m1._1` becomes `(_m1 :: any)._1`. The ship artifact keeps it.
    fn cast_root(&self, path: &str) -> String {
        if !self.options.check {
            return path.to_string();
        }

        match path.find(['.', '[']) {
            Some(i) => format!("({} :: any){}", &path[..i], &path[i..]),

            None => path.to_string(),
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

    /// The intrinsics: a closed set, resolved by name.
    fn intrinsic(&mut self, name: TokSpan, args: &[Expr], span: TokSpan) -> String {
        let n = self.text_of(name).to_string();
        let at = self.byte_start(span);
        let where_ = self.where_at(at);
        let rendered: Vec<String> = args.iter().map(|a| self.render_to_string(a)).collect();
        let sources: Vec<String> = args
            .iter()
            .map(|a| self.text_of(a.span()).to_string())
            .collect();

        match (n.as_str(), args.len()) {
            ("dbg", 1) => {
                let std = self.std();

                format!(
                    "{std}.dbg({}, {}, {})",
                    luau_string(&where_),
                    luau_string(&sources[0]),
                    rendered[0]
                )
            }

            ("todo", 0) => format!("error({})", luau_string(&format!("todo at {where_}"))),

            ("todo", 1) => format!(
                "error({} .. {})",
                luau_string(&format!("todo at {where_}: ")),
                rendered[0]
            ),

            ("unreachable", 0) => {
                format!(
                    "error({})",
                    luau_string(&format!("unreachable at {where_}"))
                )
            }

            ("assert", 1) => format!(
                "assert({}, {})",
                rendered[0],
                luau_string(&format!("assertion failed: {}", sources[0]))
            ),

            ("assert", 2) => format!("assert({}, {})", rendered[0], rendered[1]),

            ("assert_eq", 2) => {
                let std = self.std();

                format!(
                    "{std}.assert_eq({}, {}, {}, {}, {})",
                    luau_string(&where_),
                    luau_string(&sources[0]),
                    rendered[0],
                    luau_string(&sources[1]),
                    rendered[1]
                )
            }

            ("nameof", 1) => {
                let last = sources[0].rsplit(['.', ':']).next().unwrap_or(&sources[0]);

                luau_string(last.trim())
            }

            ("stringify", 1) => luau_string(&sources[0]),

            ("bnot", 1) => format!("bit32.bnot({})", rendered[0]),

            // `$matches(value, Pattern)`: the match with one arm, as a
            // boolean. The pattern is the second argument's text, read
            // by the match parser.
            ("matches", 2) => {
                let nested = format!(
                    "return match {} with case {} then true default false end",
                    sources[0], sources[1]
                );

                self.compile_fragment(&nested, at, true)
            }

            // `$set[a, b]` or `$set(a, b)`: a Set of the values.
            ("set", _) => {
                let std = self.std();
                let items: Vec<String> = match args {
                    [Expr::Array { items, .. }] => {
                        items.iter().map(|e| self.render_to_string(e)).collect()
                    }

                    _ => rendered,
                };

                format!("{std}.Set.from({{ {} }})", items.join(", "))
            }

            // `$map[[k, v], ...]` or `$map([k, v], ...)`: a HashMap of
            // the pairs.
            ("map", _) => {
                let std = self.std();
                let pairs: &[Expr] = match args {
                    [Expr::Array { items, .. }]
                        if items.iter().all(|e| matches!(e, Expr::Array { .. })) =>
                    {
                        items
                    }

                    _ => args,
                };
                let mut fields = Vec::new();
                // A table literal with string keys reads as a record, so
                // the checker learns `K` and `V` from a cast: the types
                // of the first pair, a literal's own or `typeof` of the
                // expression.
                let mut shape = None;

                for pair in pairs {
                    match pair {
                        Expr::Array { items, .. } if items.len() == 2 => {
                            let k = self.render_to_string(&items[0]);
                            let v = self.render_to_string(&items[1]);

                            if shape.is_none() {
                                let kt = literal_type(&items[0])
                                    .unwrap_or_else(|| format!("typeof({k})"));
                                let vt = literal_type(&items[1])
                                    .unwrap_or_else(|| format!("typeof({v})"));
                                shape = Some(format!("{{ [{kt}]: {vt} }}"));
                            }

                            fields.push(format!("[{k}] = {v}"));
                        }

                        other => {
                            self.diagnose(
                                other.span(),
                                "`$map` takes pairs: `$map[[key, value], [key, value]]`",
                            );
                        }
                    }
                }

                match shape {
                    Some(shape) => {
                        format!("{std}.HashMap.from({{ {} }} :: {shape})", fields.join(", "))
                    }

                    None => format!("{std}.HashMap.from({{}})"),
                }
            }

            _ => {
                self.diagnose(
                    span,
                    &format!(
                        "unknown macro or intrinsic `${n}` with {} arguments",
                        args.len()
                    ),
                );

                self.text_of(span).to_string()
            }
        }
    }

    /// `new Name(args)`, `new Name<<T>>(args)`, `new Name(args) { init }`.
    /// The head of a `new` expression before its fields table, and
    /// whether one follows: `Name.new(args)` alone, `__alloy.init(
    /// Name.new(args), ` for a call with fields, `__alloy.construct(
    /// Name, ` for fields on a name this file does not declare, which
    /// the check artifact types as `Name.new(`, the typed constructor
    /// an imported struct carries. The caller copies the table after it.
    fn new_head(
        &mut self,
        name: &Expr,
        type_args: Option<TokSpan>,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
        whole: TokSpan,
    ) -> String {
        self.check_new(name, args, init, whole);
        let ctor = self.constructor_of(name);
        let n = self.render_to_string(name);
        let t = type_args
            .map(|s| type_args_text(self.text_of(s)))
            .unwrap_or_else(|| self.expected_args_for(name));

        match (args, init) {
            (Some(a), None) => {
                let a = self.args_text(a);

                format!("{n}.{ctor}{t}{a}")
            }

            (None, None) => format!("{n}.{ctor}{t}()"),

            (Some(a), Some(_)) => {
                let a = self.args_text(a);
                let std = self.std();

                format!("{std}.init({n}.{ctor}{t}{a}, ")
            }

            (None, Some(_)) => {
                if self.options.check {
                    format!("{n}.{ctor}{t}(")
                } else {
                    let std = self.std();

                    format!("{std}.construct({n}, ")
                }
            }
        }
    }

    /// `{ ...a, x = 1, ...b }` becomes `spread(a, { x = 1 }, b)`, with the
    /// text between fields copied so the lines hold.
    fn spread_table(&mut self, span: TokSpan, fields: &[TableField]) {
        let anchor = self.byte_start(span);
        let std = self.std();
        let open = if self.options.check { "(" } else { "" };
        self.generate(anchor, &format!("{open}{std}.spread("));

        let open_end = self.toks[span.start as usize].end;
        let close_start = self.toks[span.end as usize - 1].start;
        let mut cursor = open_end;
        let mut in_group = false;
        // The parts of the merged type, in order, for the check artifact.
        let mut parts: Vec<String> = Vec::new();
        let mut group: Vec<String> = Vec::new();
        let mut spread_seen = false;

        for (i, field) in fields.iter().enumerate() {
            let (fs, fe) = self.field_bytes(field);
            let is_spread = matches!(field, TableField::Spread(_));
            let last = i + 1 == fields.len();
            let text = one_line(&self.src[fs as usize..fe as usize]);

            // The gap before a field carries the comma and the newlines.
            self.copy(cursor, fs);

            if let TableField::Spread(e) = field {
                spread_seen = true;
                self.expr(e);
                parts.push(format!("typeof({})", text.trim_start_matches("...").trim()));
            } else {
                if spread_seen && let TableField::Positional(v) = field {
                    self.diagnose(
                        v.span(),
                        "a positional entry after a spread has no place to go; name it, or move it in front of the spread",
                    );
                }

                if !in_group {
                    self.generate(fs, "{ ");
                    in_group = true;
                }

                let children = field_children(field);
                self.stitch_between(fs, fe, &children);
                group.push(text);
                let next_is_spread = matches!(fields.get(i + 1), Some(TableField::Spread(_)));

                if next_is_spread || last {
                    self.generate(fe, " }");
                    in_group = false;
                    parts.push(format!("typeof({{ {} }})", group.join(", ")));
                    group.clear();
                }
            }

            let _ = is_spread;
            cursor = fe;
        }

        // The trailing gap must not carry a comma into the call.
        let tail = &self.src[cursor as usize..close_start as usize];

        match tail.find(',') {
            Some(i) => {
                self.copy(cursor, cursor + i as u32);
                self.copy(cursor + i as u32 + 1, close_start);
            }

            None => self.copy(cursor, close_start),
        }

        // The runtime merge answers a bare table, so the check artifact
        // says what the parts add up to: a key no part names is an error.
        let cast = match (self.options.check, parts.is_empty()) {
            (true, false) => format!(") :: {})", parts.join(" & ")),
            (true, true) => "))".to_string(),
            (false, _) => ")".to_string(),
        };
        self.generate(close_start, &cast);
    }

    fn field_bytes(&self, field: &TableField) -> (u32, u32) {
        match field {
            TableField::Positional(v) => (self.byte_start(v.span()), self.byte_end(v.span())),

            TableField::Named { name, value } => {
                (self.byte_start(*name), self.byte_end(value.span()))
            }

            TableField::Computed { key, value } => (
                self.toks[key.span().start as usize - 1].start,
                self.byte_end(value.span()),
            ),

            TableField::Spread(e) => {
                let tok = self.toks[e.span().start as usize - 1];

                (tok.start, self.byte_end(e.span()))
            }
        }
    }

    // --- postfix chains ----------------------------------------------------

    /*
    Walks a postfix chain and returns its guard and inner text.

    `inner` is the expression as it stands; `guard` is the temp whose nil
    makes the whole result nil. An optional link needs its prefix named
    once, so the prefix becomes a temp (or stays, when it is simple) and
    the guard moves to it. A plain link applies inside the current guard,
    which is the chain rule: one `?` guards every later link. `!` names
    the prefix the same way, then ends the guard with an `error` branch.
    */
    fn chain_parts(&mut self, e: &Expr) -> ChainParts {
        let (base, links) = flatten(e);
        let timed_waits = self.options.wait_timeout.is_some();
        // A timed `WaitForChild` can return nil, so the link after it guards.
        let mut pending_guard = false;

        // A string literal as a receiver needs parentheses in Luau.
        let mut inner = match base {
            Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. } if !links.is_empty() => {
                format!("({})", self.render_to_string(base))
            }

            _ => self.render_to_string(base),
        };

        // `Vector3.zero(...)`: a static declared on a foreign type.
        let mut links = links;

        if let (Expr::Name(n), Some(Link::Plain(Step::Field(f)))) = (base, links.first())
            && let Some(statics) = self.ext_statics.get(self.text_of(*n))
            && statics.contains(self.text_of(*f))
        {
            let target = self.text_of(*n);

            if !self.options.check {
                let std = self.std();
                inner = format!(
                    "{std}.static({}, {})",
                    luau_string(target),
                    luau_string(self.text_of(*f))
                );
                self.ext_hit = true;
                links.remove(0);
            } else if PRIMITIVES.contains(&target) {
                inner = format!("__alloy_{target}.{}", self.text_of(*f));
                links.remove(0);
            }
        }
        // `import(...)` is `require(...)`. A string or an instance chain
        // types itself; a dynamic path is `unknown` unless `<<T>>` says.
        if self.is_import_call(e)
            && let Some(Link::Plain(Step::Call {
                type_args, args, ..
            })) = links.first()
        {
            // A data path drops its extension, as in `import` statements.
            let a = match args {
                CallArgs::Str(s) => crate::data::strip_literal(self.text_of(*s)),

                CallArgs::Paren(list) if list.len() == 1 && matches!(list[0], Expr::String(_)) => {
                    crate::data::strip_literal(&self.render_to_string(&list[0]))
                }

                _ => self.args_text(args),
            };
            let a = if a.starts_with('(') {
                a
            } else {
                format!("({a})")
            };
            let is_static = match args {
                CallArgs::Str(_) => true,

                CallArgs::Paren(list) => list.len() == 1 && is_static_module(&list[0]),

                CallArgs::Table(_) => false,
            };
            let ty = type_args.map(|s| {
                array_types(
                    self.text_of(s)
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .trim(),
                )
            });
            inner = match ty {
                Some(t) => format!("(require{a} :: {t})"),

                None if is_static => format!("require{a}"),

                None => format!("(require{a} :: unknown)"),
            };
            links.remove(0);
        }

        let mut inner_simple = self.is_simple(base);
        let mut guard: Option<String> = None;

        // `HashMap.new()` under `local m: HashMap<K, V>`: the arguments
        // the annotation names go on the call.
        if let (Expr::Name(n), Some((base_name, args_text))) = (base, self.expected_generic.clone())
            && self.text_of(*n) == base_name
            && let [
                Link::Plain(Step::Field(f)),
                Link::Plain(Step::Call {
                    method: None,
                    type_args: None,
                    args,
                }),
            ] = links.as_slice()
            && matches!(self.text_of(*f), "new" | "from" | "with_capacity")
        {
            let method = self.text_of(*f).to_string();
            let a = self.args_text(args);

            let targs = type_args_text(&format!("<<{args_text}>>"));

            return ChainParts {
                guard: None,
                inner: format!("{inner}.{method}{targs}{a}"),
            };
        }

        for link in links {
            let link = match link {
                Link::Plain(step) if pending_guard => Link::Optional(step),

                other => other,
            };
            pending_guard = false;

            match link {
                Link::Plain(step) => {
                    inner_simple = inner_simple && matches!(step, Step::Field(_));
                    pending_guard = timed_waits && matches!(step, Step::Child { wait: true, .. });
                    inner = self.apply(&inner, &step);
                }

                Link::Optional(step) => {
                    let name = self.name_prefix(&mut inner, &mut guard, inner_simple);
                    inner_simple = false;
                    pending_guard = timed_waits && matches!(step, Step::Child { wait: true, .. });
                    // `f?()` on a field of an optional function type: the new
                    // solver loses the field's type under an earlier `== n`
                    // refinement of a metatable-typed `self`, and reports an
                    // error type at the call. The check artifact calls
                    // through `any`; the guard on the value stays typed.
                    let callee =
                        if self.options.check && matches!(step, Step::Call { method: None, .. }) {
                            format!("({name} :: any)")
                        } else {
                            name
                        };
                    inner = self.apply(&callee, &step);
                }

                Link::NonNil { span } => {
                    let name = self.name_prefix(&mut inner, &mut guard, inner_simple);
                    let source = self.text_of(span);
                    let message = luau_string(&format!("{source} is nil"));
                    inner = format!("(if {name} == nil then error({message}) else {name})");
                    inner_simple = false;
                    // `!` ends the guard: past it the value is never nil.
                    guard = None;
                }
            }
        }

        ChainParts { guard, inner }
    }

    /// Names the current prefix so a link can test it and then use it.
    fn name_prefix(
        &mut self,
        inner: &mut String,
        guard: &mut Option<String>,
        inner_simple: bool,
    ) -> String {
        let name = if guard.is_none() && inner_simple {
            inner.clone()
        } else {
            let whole = self.guarded(guard.as_deref(), inner);
            let anchor = self.chain_anchor;
            self.hoist_text(whole, anchor)
        };

        *guard = Some(name.clone());
        *inner = name.clone();

        name
    }

    fn guarded(&self, guard: Option<&str>, inner: &str) -> String {
        match guard {
            Some(g) => format!("(if {g} == nil then nil else {inner})"),

            None => inner.to_string(),
        }
    }

    fn apply(&mut self, prefix: &str, step: &Step<'_>) -> String {
        match step {
            Step::Field(name) => format!("{prefix}.{}", self.text_of(*name)),

            Step::Computed(key) => {
                let k = self.render_to_string(key);

                format!("{prefix}[{k}]")
            }

            Step::Call {
                method,
                type_args,
                args,
            } => {
                // A method name declared on a foreign type in this file
                // routes through the dispatcher, which falls back to the
                // receiver's own method.
                if let Some(m) = method {
                    let mname = self.text_of(*m).to_string();

                    if self.ext_methods.contains(&mname) && type_args.is_none() {
                        let a = self.args_text(args);
                        let inner = a.trim_start_matches('(').trim_end_matches(')');
                        let sep = if inner.is_empty() { "" } else { ", " };

                        if !self.options.check {
                            let std = self.std();
                            self.ext_hit = true;

                            return format!("{std}.call({prefix}, \"{mname}\"{sep}{inner})");
                        }

                        if let Some(target) = self.ext_primitive.get(&mname) {
                            return format!("__alloy_{target}.{mname}({prefix}{sep}{inner})");
                        }
                    }
                }

                let m = match method {
                    Some(m) => format!(":{}", self.text_of(*m)),

                    None => String::new(),
                };
                let t = type_args
                    .map(|s| type_args_text(self.text_of(s)))
                    .unwrap_or_default();
                // `Signal.new<T...>` takes a type pack, not a list of
                // type parameters, so its arguments go in parentheses.
                let t = if prefix == "__alloy.Signal.new" {
                    pack_type_args(&t)
                } else {
                    t
                };
                let a = self.args_text(args);

                format!("{prefix}{m}{t}{a}")
            }

            Step::Child { name, wait } => {
                let n = match name {
                    ChildName::Name(s) => luau_string(self.text_of(*s)),

                    ChildName::Str(s) => self.text_of(*s).to_string(),

                    ChildName::Computed(e) => self.render_to_string(e),
                };

                let call = match (*wait, self.options.wait_timeout) {
                    (true, Some(t)) => format!("{prefix}:WaitForChild({n}, {})", luau_number(t)),

                    (true, None) => format!("{prefix}:WaitForChild({n})"),

                    (false, _) => format!("{prefix}:FindFirstChild({n})"),
                };

                // The checker types the child as `Instance`, which has no
                // `CFrame`; the source names no class, so the check
                // artifact lets the chain continue untyped.
                if self.options.check {
                    format!("({call} :: any)")
                } else {
                    call
                }
            }
        }
    }

    fn args_text(&mut self, args: &CallArgs) -> String {
        match args {
            CallArgs::Paren(list) => {
                let parts: Vec<String> = list.iter().map(|e| self.render_to_string(e)).collect();

                format!("({})", parts.join(", "))
            }

            CallArgs::Table(t) => self.render_to_string(t),

            CallArgs::Str(s) => self.text_of(*s).to_string(),
        }
    }

    /// A chain in expression position.
    fn chain_expr(&mut self, e: &Expr) -> String {
        self.chain_anchor = self.byte_start(e.span());
        self.check_struct_call(e);
        let parts = self.chain_parts(e);

        self.guarded(parts.guard.as_deref(), &parts.inner)
    }

    /// A struct constructs through `new` alone. `Vec2 { ... }` and
    /// `Vec2(1, 2)` are diagnostics: the first reads as a call with a
    /// table, the second calls the class, which takes the fields table. A
    /// foreign class has no class call of Alloy's, so `new` stays optional
    /// there.
    fn check_struct_call(&mut self, e: &Expr) {
        let (base, links) = flatten(e);

        if let Expr::Name(n) = base
            && self.is_struct_call(e)
        {
            let name = self.text_of(*n).to_string();
            let raw = matches!(
                links.first(),
                Some(Link::Plain(Step::Call {
                    args: CallArgs::Table(_),
                    ..
                }))
            );
            let ctor = self.structs_with_new.get(&name).cloned();
            let message = match (raw, ctor) {
                (true, Some(_)) => {
                    format!("`{name}` writes a constructor: construct it with `new {name}(...)`")
                }

                (true, None) => format!("construct `{name}` with `new {name} {{ ... }}`"),

                (false, Some(_)) => {
                    format!("`{name}(...)` is not a call: construct it with `new {name}(...)`")
                }

                (false, None) => format!(
                    "`{name}(...)` is not a constructor: construct it with `new {name} {{ ... }}`, since `{name}` writes no `new`"
                ),
            };
            self.diagnostics.push(Diagnostic {
                start: self.byte_start(*n),
                end: self.byte_end(*n),
                message,
            });
        }
    }

    /// `new Name { ... }` with no parentheses on a struct: the table is
    /// the fields, and the class call takes it.
    fn fields_form(&self, name: &Expr, args: Option<&CallArgs>, init: Option<&Expr>) -> bool {
        let Expr::Name(n) = name else {
            return false;
        };

        args.is_none() && init.is_some() && self.structs.contains(self.text_of(*n))
    }

    /// The constructor `new Name(...)` calls: the `new` or `New` the impl
    /// wrote, else `new`, which a foreign class or an imported struct has.
    fn constructor_of(&self, name: &Expr) -> String {
        match name {
            Expr::Name(n) => self
                .structs_with_new
                .get(self.text_of(*n))
                .cloned()
                .unwrap_or_else(|| "new".to_string()),

            _ => "new".to_string(),
        }
    }

    /// Records a struct's name and fields for construction checks.
    fn note_struct(&mut self, st: &StructDecl) {
        let name = self.text_of(st.name).to_string();
        let fields = st
            .fields
            .iter()
            .map(|f| (self.text_of(f.name).to_string(), f.default.is_some()))
            .collect();
        self.structs.insert(name.clone());
        self.note_field_types(&name, &st.fields);
        let wire_fields = st
            .fields
            .iter()
            .map(|f| WireField {
                name: self.text_of(f.name).to_string(),
                ty: self.text_of(f.ty).trim().to_string(),
                width: f.attributes.iter().find_map(|a| {
                    let n = self.text_of(a.name?).to_string();

                    WIRE_WIDTHS.contains(&n.as_str()).then_some(n)
                }),
            })
            .collect();
        self.struct_wire.insert(name.clone(), wire_fields);

        if st.generics.is_some() {
            self.generic_types.insert(name.clone());
        }

        if st
            .fields
            .iter()
            .any(|f| f.visibility.is_some_and(|v| self.text_of(v) == "private"))
        {
            self.private_types.insert(name.clone());
        }

        self.struct_fields.insert(name, fields);
    }

    /// `Partial<S>` at an ambient name token, with `S` declared here:
    /// the expanded table and the byte after the closing `>`.
    fn mapped_over_declared(&mut self, span: TokSpan, end: u32) -> Option<(String, u32)> {
        let name = self.text_of(span).to_string();

        if !matches!(name.as_str(), "Partial" | "Readonly" | "Sink") {
            return None;
        }

        let i = span.end as usize;
        let lt = self.toks.get(i)?;
        let target = self.toks.get(i + 1)?;
        let gt = self.toks.get(i + 2)?;

        if lt.text(self.src) != "<" || target.kind != TokKind::Ident || gt.end > end {
            return None;
        }

        let closes = gt.text(self.src) == ">" || gt.text(self.src) == ">>";

        if !closes {
            return None;
        }

        let target_name = target.text(self.src).to_string();
        let table = self.inline_mapped(&name, &target_name)?;
        // `>>` closes an outer list too; only the first `>` is this one.
        let after = if gt.text(self.src) == ">>" {
            gt.start + 1
        } else {
            gt.end
        };

        Some((table, after))
    }

    /// The annotation's base and arguments when a one-name local with a
    /// std container annotation holds that container's constructor call
    /// and nothing else: `Base.new()`, `Base.from(x)`, `new Base()`.
    fn annotated_constructor(&self, l: &Local) -> Option<(String, String)> {
        if l.names.len() != 1 || l.values.len() != 1 || l.names[0].destructure.is_some() {
            return None;
        }

        let head = generic_head(self.text_of(l.names[0].ty?))?;
        let value = &l.values[0];

        let is_ctor = match value {
            Expr::New {
                name,
                type_args: None,
                ..
            } => matches!(name.as_ref(), Expr::Name(n) if self.text_of(*n) == head.0),

            _ => {
                let (base, links) = flatten(value);

                matches!(base, Expr::Name(n) if self.text_of(*n) == head.0)
                    && matches!(
                        links.as_slice(),
                        [
                            Link::Plain(Step::Field(f)),
                            Link::Plain(Step::Call {
                                method: None,
                                type_args: None,
                                ..
                            })
                        ] if matches!(self.text_of(*f), "new" | "from" | "with_capacity")
                    )
            }
        };

        is_ctor.then_some(head)
    }

    /// `<K, V>` for `new Base(...)` under an annotation `Base<K, V>`.
    fn expected_args_for(&self, name: &Expr) -> String {
        match (name, &self.expected_generic) {
            (Expr::Name(n), Some((base, args))) if self.text_of(*n) == base => {
                type_args_text(&format!("<<{args}>>"))
            }

            _ => String::new(),
        }
    }

    fn note_field_types(&mut self, name: &str, fields: &[Field]) {
        let types = fields
            .iter()
            .map(|f| FieldType {
                name: self.text_of(f.name).to_string(),
                ty: f.ty,
                private: f.visibility.is_some_and(|v| self.text_of(v) == "private"),
            })
            .collect();
        self.struct_field_types.insert(name.to_string(), types);
    }

    /// `Partial<S>`, `Readonly<S>`, `Sink<S>` over a struct or an
    /// interface declared here, as a plain table type. The solver's type
    /// functions cannot reduce a type that holds `Array<T>` or another
    /// recursive generic, and the fields are known, so the check
    /// artifact writes them out. `None` when the name is not one of
    /// these or the argument is not a declared type.
    fn inline_mapped(&mut self, mapped: &str, target: &str) -> Option<String> {
        let (prefix, optional) = match mapped {
            "Partial" => ("", "?"),
            "Readonly" => ("read ", ""),
            "Sink" => ("write ", ""),
            _ => return None,
        };
        let fields = self.struct_field_types.get(target)?.clone();
        let mut parts = Vec::new();

        for f in fields.iter().filter(|f| !f.private) {
            let ty = self.copy_type_to_string(f.ty);
            let ty = ty.trim();
            let opt = if optional.is_empty() || ty.ends_with('?') {
                ""
            } else {
                optional
            };
            parts.push(format!("{prefix}{}: {ty}{opt}", f.name));
        }

        Some(format!("{{ {} }}", parts.join(", ")))
    }

    /// Whether the check artifact splits a struct into a public view and
    /// a full one: it has a private member and no type parameters.
    fn has_private_view(&self, name: &str) -> bool {
        self.options.check
            && self.private_types.contains(name)
            && !self.generic_types.contains(name)
    }

    /// The type of `self` inside a struct's own code: the full view when
    /// the struct has private members, else the struct.
    /// What the check artifact adds to an `if` so the checker sees an
    /// `is` test on a type it cannot refine: a struct, an enum, an alias
    /// datatype. Each branch that tests `x is T` on a plain name starts
    /// with `local x = ((x :: any) :: T)`; `x is not T` types the else
    /// branch, or the code after a guard that leaves.
    fn narrowings(&self, i: &If) -> (Vec<(u32, String)>, Option<String>) {
        let mut blocks = Vec::new();

        if !self.options.check {
            return (blocks, None);
        }

        for (cond, block) in &i.branches {
            let Cond::Expr(e) = cond else {
                continue;
            };
            let mut tests = Vec::new();
            self.positive_tests(e, &mut tests);
            let prefix: String = tests
                .iter()
                .map(|(name, ty)| format!("local {name} = (({name} :: any) :: {ty}) "))
                .collect();

            if !prefix.is_empty() {
                blocks.push((block.span.start, prefix));
            }
        }

        let mut after = None;

        if i.branches.len() == 1
            && let Cond::Expr(e) = &i.branches[0].0
            && let Some((name, ty)) = self.negative_test(e)
        {
            let text = format!("local {name} = (({name} :: any) :: {ty})");

            match &i.else_block {
                Some(b) => blocks.push((b.span.start, format!("{text} "))),

                None if self.block_leaves(&i.branches[0].1) => after = Some(format!(" {text}")),

                None => {}
            }
        }

        (blocks, after)
    }

    /// The `x is T` tests an `and` chain holds, as (name, type).
    fn positive_tests(&self, e: &Expr, out: &mut Vec<(String, String)>) {
        match e {
            Expr::Paren { inner, .. } => self.positive_tests(inner, out),

            Expr::Binary { op, lhs, rhs, .. } if self.text_of(*op) == "and" => {
                self.positive_tests(lhs, out);
                self.positive_tests(rhs, out);
            }

            Expr::Is {
                expr,
                negated: false,
                name,
                ..
            } => {
                if let Expr::Name(n) = &**expr
                    && let Some(ty) = self.narrow_type(self.text_of(*name), self.text_of(*n))
                {
                    out.push((self.text_of(*n).to_string(), ty));
                }
            }

            _ => {}
        }
    }

    /// `x is not T`, or `not (x is T)`, as (name, type).
    fn negative_test(&self, e: &Expr) -> Option<(String, String)> {
        match e {
            Expr::Paren { inner, .. } => self.negative_test(inner),

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "not" => {
                let mut tests = Vec::new();
                self.positive_tests(operand, &mut tests);

                (tests.len() == 1).then(|| tests.remove(0))
            }

            Expr::Is {
                expr,
                negated: true,
                name,
                ..
            } => match &**expr {
                Expr::Name(n) => {
                    let ty = self.narrow_type(self.text_of(*name), self.text_of(*n))?;

                    Some((self.text_of(*n).to_string(), ty))
                }

                _ => None,
            },

            _ => None,
        }
    }

    /// The type an `is` narrows `value` to when Luau cannot: a struct,
    /// an enum, an imported type, or a datatype the definitions declare
    /// as an alias. A class, a primitive, and a datatype class refine on
    /// their own; `table` and `function` refine to the top types, which
    /// no index or call accepts, so those meet the value's own type.
    fn narrow_type(&self, name: &str, value: &str) -> Option<String> {
        match name {
            "table" => return Some(format!("typeof({value}) & {{ [any]: any }}")),

            // Both function shapes: a call yields values, and a callback
            // parameter that returns nothing accepts it.
            "function" => {
                return Some(format!(
                    "typeof({value}) & ((...any) -> ...any) & ((...any) -> ())"
                ));
            }

            _ => {}
        }

        if self.generic_types.contains(name) {
            return None;
        }

        if self.structs.contains(name) {
            return Some(if self.impl_target.as_deref() == Some(name) {
                self.self_alias(name)
            } else {
                name.to_string()
            });
        }

        if self.enums.contains_key(name)
            || self
                .options
                .import_types
                .iter()
                .any(|(_, names)| names.iter().any(|n| n == name))
            || ALIAS_DATATYPES.contains(&name)
        {
            return Some(name.to_string());
        }

        None
    }

    /// Whether a block ends in a statement that leaves it: a return, a
    /// break, a continue, or an `error` call.
    fn block_leaves(&self, block: &Block) -> bool {
        match block.stmts.last() {
            Some(Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_)) => true,

            Some(Stmt::Call(
                Expr::Call {
                    func, method: None, ..
                },
                _,
            )) => {
                matches!(&**func, Expr::Name(n) if self.text_of(*n) == "error")
            }

            _ => false,
        }
    }

    fn self_alias(&self, name: &str) -> String {
        if self.has_private_view(name) {
            format!("{name}__all")
        } else {
            name.to_string()
        }
    }

    /// The fields form names every field without a default and no field
    /// the struct lacks. A spread or a computed key turns the check off,
    /// because the table's keys are then not in the source.
    fn check_struct_fields(&mut self, name: &Expr, table: &Expr) {
        let Expr::Name(n) = name else {
            return;
        };
        let sname = self.text_of(*n).to_string();
        let Some(declared) = self.struct_fields.get(&sname).cloned() else {
            return;
        };
        let Expr::Table { fields, .. } = table else {
            return;
        };
        let mut given: Vec<String> = Vec::new();
        let mut open = false;

        for f in fields {
            match f {
                TableField::Named { name, .. } => {
                    let fname = self.text_of(*name).to_string();

                    if !declared.iter().any(|(d, _)| *d == fname) {
                        let known: Vec<&str> = declared.iter().map(|(d, _)| d.as_str()).collect();
                        self.diagnose(
                            *name,
                            &format!(
                                "`{sname}` has no field `{fname}`; its fields are {}",
                                list_names(&known)
                            ),
                        );
                    }

                    given.push(fname);
                }

                _ => open = true,
            }
        }

        if open {
            return;
        }

        let missing: Vec<&str> = declared
            .iter()
            .filter(|(d, has_default)| !has_default && !given.contains(d))
            .map(|(d, _)| d.as_str())
            .collect();

        if !missing.is_empty() {
            self.diagnose(
                *n,
                &format!(
                    "`new {sname} {{ ... }}` leaves {} unset; a field without a default needs a value",
                    list_names(&missing)
                ),
            );
        }
    }

    /// The two mistakes with `new` on a struct: the fields form on one that
    /// writes a constructor, outside that constructor's impl, and
    /// parentheses on one that writes none.
    fn check_new(
        &mut self,
        name: &Expr,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
        whole: TokSpan,
    ) {
        let Expr::Name(n) = name else {
            return;
        };
        let text = self.text_of(*n).to_string();

        // A name that is not a value with a constructor: the whole
        // expression is the error, since none of it can run.
        let what = if self.enums.contains_key(&text) && !self.structs.contains(&text) {
            Some("enum")
        } else {
            self.not_constructible.get(&text).copied()
        };

        if let Some(what) = what {
            let hint = match what {
                "enum" => format!("pick a variant: `{text}.Variant(...)`"),
                "attribute" => format!("write it above a declaration: `@{text}(...)`"),
                "trait" => "construct a struct that implements it".to_string(),
                "interface" => "construct a struct that has its fields".to_string(),
                _ => "a remote is called, not constructed".to_string(),
            };
            self.diagnostics.push(Diagnostic {
                start: self.byte_start(whole),
                end: self.byte_end(whole),
                message: format!(
                    "`{text}` is an {what} and cannot be constructed with `new`; {hint}"
                )
                .replace("an trait", "a trait")
                .replace("an remote", "a remote"),
            });

            return;
        }

        if !self.structs.contains(&text) {
            return;
        }

        let ctor = self.structs_with_new.get(&text).cloned();

        let message = if self.fields_form(name, args, init) {
            match ctor {
                Some(ctor) if self.impl_target.as_deref() != Some(text.as_str()) => format!(
                    "`{text}` writes `{ctor}`: construct it with `new {text}(...)`; the fields form is the constructor's own"
                ),

                _ => {
                    if let Some(table) = init {
                        self.check_struct_fields(name, table);
                    }

                    return;
                }
            }
        } else {
            match ctor {
                Some(_) => return,

                None => format!(
                    "`{text}` writes no `new` or `New`: construct it with `new {text} {{ ... }}`, or write `function new` in `impl {text}`"
                ),
            }
        };

        self.diagnostics.push(Diagnostic {
            start: self.byte_start(*n),
            end: self.byte_end(*n),
            message,
        });
    }

    /// Whether a plain function's parameter list carries `@attr`. The
    /// parser keeps the text; only a remote reads a wire width.
    fn params_have_attrs(&self, body: &FunctionBody) -> bool {
        !self.param_attr_spans(body).is_empty()
    }

    /// The `@name` texts in front of parameters, with their byte ranges.
    fn param_attr_spans(&self, body: &FunctionBody) -> Vec<(u32, u32, String)> {
        let open = self.toks[self.params_open_tok(body) as usize].end;
        let mut found = Vec::new();

        for (i, p) in body.params.iter().enumerate() {
            let from = if i == 0 {
                open
            } else {
                self.byte_end(body.params[i - 1].name)
            };
            let gap = &self.src[from as usize..self.byte_start(p.name) as usize];

            if let Some(at) = gap.find('@') {
                let name: String = gap[at + 1..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let start = from + at as u32;
                found.push((start, start + 1 + name.len() as u32, name));
            }
        }

        found
    }

    fn check_param_attrs(&mut self, body: &FunctionBody) {
        for (start, end, name) in self.param_attr_spans(body) {
            self.diagnostics.push(Diagnostic {
                start,
                end,
                message: format!(
                    "`@{name}` applies to a remote parameter or a struct field, not a function parameter"
                ),
            });
        }
    }

    /// `import(...)`: the reserved word called, which the parser lets
    /// through for this form alone.
    fn is_import_call(&self, e: &Expr) -> bool {
        let (base, links) = flatten(e);

        matches!(base, Expr::Name(n) if self.text_of(*n) == "import")
            && matches!(
                links.first(),
                Some(Link::Plain(Step::Call { method: None, .. }))
            )
    }

    /// `Name(...)` with a struct's name and no fields table, or the fields
    /// form on a struct that writes `new`, outside its own impl.
    fn is_struct_call(&self, e: &Expr) -> bool {
        let (base, links) = flatten(e);
        let Expr::Name(n) = base else {
            return false;
        };
        let name = self.text_of(*n);

        if !self.structs.contains(name) {
            return false;
        }

        matches!(
            links.first(),
            Some(Link::Plain(Step::Call { method: None, .. }))
        )
    }

    /// A chain that is a call statement: the guard becomes an `if`.
    fn chain_stmt(&mut self, e: &Expr) -> String {
        self.chain_anchor = self.byte_start(e.span());
        self.check_struct_call(e);
        let parts = self.chain_parts(e);

        match parts.guard {
            Some(g) => format!("if {g} ~= nil then {} end", parts.inner),

            // A statement that opens with `(` reads as a call of the line
            // above in Luau. Inside `do ... end` it is the first statement
            // of a block, so nothing precedes it to be called.
            None if parts.inner.starts_with('(') => format!("do {} end", parts.inner),

            None => parts.inner,
        }
    }

    // --- assignment --------------------------------------------------------

    fn assign_needs_rewrite(&self, a: &Assign) -> bool {
        self.is_coalesce_assign(a.op) || a.targets.iter().any(chain_has_alloy)
    }

    /*
    An assignment whose target is an optional chain, or whose operator is
    `??=`.

    `a?.b.c = v` becomes `if G ~= nil then INNER = v end`, with the value
    inside the guard so it does not evaluate when the chain is nil. `t ??= v`
    becomes `if T == nil then T = v end`, where `T` reads twice, so a
    computed key or a call in the target hoists first.
    */
    fn assign(&mut self, a: &Assign) {
        let anchor = self.byte_start(a.span);

        if a.targets.len() != 1 || a.values.len() != 1 {
            self.diagnose(
                a.span,
                "an assignment through `?` or with `??=` takes one target and one value",
            );
            self.copy_span(a.span);

            return;
        }

        let coalesce = self.is_coalesce_assign(a.op);
        self.chain_anchor = anchor;
        let (guard, target) = self.target_parts(&a.targets[0], coalesce);
        let value = self.render_to_string(&a.values[0]);
        let op = if coalesce { "=" } else { self.text_of(a.op) };

        let body = if coalesce {
            format!("if {target} == nil then {target} = {value} end")
        } else {
            format!("{target} {op} {value}")
        };

        let text = match guard {
            Some(g) => format!("if {g} ~= nil then {body} end"),

            None => body,
        };

        self.generate(anchor, &text);
    }

    /// The guard and the assignable text of a target. With `twice`, the
    /// object and key of the last link become names safe to read twice.
    fn target_parts(&mut self, target: &Expr, twice: bool) -> (Option<String>, String) {
        let Expr::Index { object, key, .. } = target else {
            // A plain name, or something the parser let through as a target.
            return (None, self.render_to_string(target));
        };

        let parts = self.chain_parts(object);
        let mut guard = parts.guard;
        let mut obj = parts.inner;

        let optional = matches!(target, Expr::Index { optional: true, .. });
        let simple = guard.is_none() && self.is_simple(object);

        if optional {
            let name = self.name_prefix(&mut obj, &mut guard, simple);
            obj = name;
        } else if twice && !(guard.is_none() && self.is_simple(object)) {
            let whole = self.guarded(guard.as_deref(), &obj);
            let anchor = self.chain_anchor;
            obj = self.hoist_text(whole, anchor);
            guard = None;
        }

        let text = match key {
            IndexKey::Field(name) => format!("{obj}.{}", self.text_of(*name)),

            IndexKey::Computed(k) => {
                let k = if twice {
                    self.reusable(k)
                } else {
                    self.render_to_string(k)
                };

                format!("{obj}[{k}]")
            }
        };

        (guard, text)
    }

    // --- locals with destructuring or an initializer ----------------------

    /*
    `local { a, b = c }: T = t` becomes `local _1: T = t local a, c = _1.a,
    _1.b`. `local x = new X(args) { fields }` becomes `local _1 = X.new(args)`
    then one `_1.field = value` per field, then `local x = _1` on the line of
    the closing brace, so each field traces to its own line.
    */
    fn local_stmt(&mut self, l: &Local) {
        let anchor = self.byte_start(l.span);

        if let (
            1,
            1,
            Some(Expr::New {
                name,
                type_args,
                args,
                init: Some(init),
                span,
            }),
        ) = (l.names.len(), l.values.len(), l.values.first())
            && l.names[0].destructure.is_none()
            && !self.fields_form(name, args.as_ref(), Some(init))
        {
            self.local_new_init(l, name, *type_args, args.as_ref(), init, *span, anchor);

            return;
        }

        if l.names.len() != l.values.len() {
            self.diagnose(l.span, "destructuring needs one value per binding");
            self.copy_span(l.span);

            return;
        }

        let keyword = self.text_of(l.keyword).to_string();
        let mut names = Vec::new();
        let mut values = Vec::new();
        let mut decls: Vec<String> = Vec::new();

        for (b, v) in l.names.iter().zip(&l.values) {
            self.expected_generic = b.ty.and_then(|t| generic_head(self.text_of(t)));
            let value = self.render_to_string(v);
            self.expected_generic = None;
            let ty =
                b.ty.map(|t| format!(": {}", self.text_of(t)))
                    .unwrap_or_default();

            match &b.destructure {
                None => {
                    names.push(format!("{}{ty}", self.text_of(b.name)));
                    values.push(value);
                }

                Some(d) => {
                    let typed = match b.ty {
                        Some(t) => format!("{value} :: {}", self.text_of(t)),

                        None => value,
                    };
                    let temp = self.hoist_text(typed, anchor);
                    let (ns, vs) = self.destructure_parts(d, &temp);
                    decls.push(format!("{keyword} {ns} = {vs}"));
                }
            }
        }

        let mut text = String::new();

        if !names.is_empty() {
            text.push_str(&format!(
                "{keyword} {} = {}",
                names.join(", "),
                values.join(", ")
            ));
        }

        for d in decls {
            if !text.is_empty() {
                text.push(' ');
            }

            text.push_str(&d);
        }

        self.generate(anchor, &text);
    }

    #[allow(clippy::too_many_arguments)]
    fn local_new_init(
        &mut self,
        l: &Local,
        name: &Expr,
        type_args: Option<TokSpan>,
        args: Option<&CallArgs>,
        init: &Expr,
        whole: TokSpan,
        anchor: u32,
    ) {
        // The binding itself holds the instance from the first line, so
        // a hover on its name finds a local there, and each field line
        // assigns through the name. `const` is a `local` in Luau.
        let ty = match l.names[0].ty {
            Some(t) => format!(": {}", self.text_of(t)),

            None => String::new(),
        };
        let binding = self.text_of(l.names[0].name).to_string();
        self.new_init(
            &format!("local {binding}{ty}"),
            &binding,
            name,
            type_args,
            args,
            init,
            whole,
            anchor,
        );
    }

    /// The initializer form: `decl = X.new(args)` on the first line, then
    /// one `binding.field = value` per field line.
    #[allow(clippy::too_many_arguments)]
    fn new_init(
        &mut self,
        decl: &str,
        binding: &str,
        name: &Expr,
        type_args: Option<TokSpan>,
        args: Option<&CallArgs>,
        init: &Expr,
        whole: TokSpan,
        anchor: u32,
    ) {
        self.check_new(name, args, Some(init), whole);
        let ctor = self.constructor_of(name);
        let n = self.render_to_string(name);
        let t = type_args
            .map(|s| type_args_text(self.text_of(s)))
            .unwrap_or_default();
        // With no arguments the name is one this file did not declare:
        // the runtime constructs it, and the check artifact types the
        // constructor call so the field lines below check against it.
        let head = match args {
            Some(a) => {
                let a = self.args_text(a);

                format!("{n}.{ctor}{t}{a}")
            }

            None if self.options.check => format!("{n}.{ctor}{t}({{}} :: any)"),

            None => {
                let std = self.std();

                format!("{std}.construct({n}, {{}})")
            }
        };
        self.generate(anchor, &format!("{decl} = {head}"));

        let Expr::Table { fields, span } = init else {
            unreachable!("the parser only builds a table initializer");
        };

        let open_end = self.toks[span.start as usize].end;
        let close = self.toks[span.end as usize - 1];
        let mut cursor = open_end;

        for field in fields {
            let (fs, fe) = self.field_bytes(field);
            // The gap holds the newlines; the comma becomes a space since
            // the fields are statements now.
            self.copy_gap_without_commas(cursor, fs);

            match field {
                TableField::Named { name, value } => {
                    let f = self.text_of(*name).to_string();
                    self.generate(fs, &format!("{binding}.{f} = "));
                    self.expr(value);
                }

                TableField::Computed { key, value } => {
                    let k = self.render_to_string(key);
                    self.generate(fs, &format!("{binding}[{k}] = "));
                    self.expr(value);
                }

                other => {
                    self.diagnose(*span, "an initializer takes `name = value` fields only");
                    let children = field_children(other);
                    self.stitch_between(fs, fe, &children);
                }
            }

            cursor = fe;
        }

        self.copy_gap_without_commas(cursor, close.start);
        let _ = close;
    }

    /// Copies a gap, turning each comma into a space so newlines survive.
    fn copy_gap_without_commas(&mut self, start: u32, end: u32) {
        let mut cursor = start;
        let gap = &self.src[start as usize..end as usize];

        for (i, b) in gap.bytes().enumerate() {
            if b == b',' {
                let at = start + i as u32;
                self.copy(cursor, at);
                self.generate(at, " ");
                cursor = at + 1;
            }
        }

        self.copy(cursor, end);
    }

    /// The names and values of a destructure over a temp.
    fn destructure_parts(&mut self, d: &Destructure, temp: &str) -> (String, String) {
        match d {
            Destructure::Table(fields) => {
                let ns: Vec<String> = fields
                    .iter()
                    .map(|f| self.text_of(f.rename.unwrap_or(f.field)).to_string())
                    .collect();
                let vs: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{temp}.{}", self.text_of(f.field)))
                    .collect();

                (ns.join(", "), vs.join(", "))
            }

            Destructure::Array { items, rest } => {
                let mut ns: Vec<String> =
                    items.iter().map(|i| self.text_of(*i).to_string()).collect();
                let mut vs: Vec<String> =
                    (1..=items.len()).map(|i| format!("{temp}[{i}]")).collect();

                if let Some(r) = rest {
                    ns.push(self.text_of(*r).to_string());
                    let std = self.std();
                    vs.push(format!("{std}.Array.slice({temp}, {})", items.len() + 1));
                }

                (ns.join(", "), vs.join(", "))
            }
        }
    }

    // --- functions ---------------------------------------------------------

    /*
    A function with Alloy in its header: `async`, a `->` return type, a
    default, or a destructured parameter.

    The header copies with the `async` token dropped, `->` replaced by `:`,
    and a defaulted parameter's type made optional. A prologue right after
    the header rebinds defaults and unpacks destructures on the same line.
    An async body wraps in `return __alloy.future(function(...) ... end,
    ...)`, so the function returns a Future and the body runs on its own
    thread.
    */
    fn function_with_header(&mut self, span: TokSpan, body: &FunctionBody) {
        let start = self.byte_start(span);
        let end_tok = self.toks[span.end as usize - 1];
        let params_open = self.toks[self.params_open_tok(body) as usize];
        let mut cursor = start;

        // 1. The header up to `(`, minus the `async` token.
        if let Some(a) = body.is_async {
            let as_ = self.byte_start(a);
            self.copy(cursor, as_);
            cursor = self.toks[a.end as usize].start;
        }

        // A bound `<T: Shape>` has no Luau form: the generic list loses it
        // and each parameter typed `T` becomes `(T & Shape)`.
        let bounds: Vec<(String, String)> = body
            .generics
            .map(|g| generic_bounds(self.text_of(g)))
            .unwrap_or_default()
            .into_iter()
            .map(|(n, b)| {
                let resolved = self.resolve_bound(&b);

                (n, resolved)
            })
            .collect();

        if let Some(g) = body.generics
            && !bounds.is_empty()
        {
            let gs = self.byte_start(g);
            let ge = self.byte_end(g);
            self.copy(cursor, gs);
            let stripped = strip_bounds(self.text_of(g));
            self.generate(gs, &stripped);
            cursor = ge;
        }

        self.copy(cursor, params_open.end);
        cursor = params_open.end;

        // 2. Parameters: a destructure becomes a temp, a default makes the
        //    type optional, and both add a prologue line.
        let mut prologue: Vec<String> = Vec::new();
        let mut param_temp = 0;

        for p in &body.params {
            let ps = self.byte_start(p.name);
            self.copy(cursor, ps);

            match &p.destructure {
                Some(d) => {
                    param_temp += 1;
                    let temp = format!("_p{param_temp}");
                    self.generate(ps, &temp);
                    let (ns, vs) = self.destructure_parts(d, &temp);
                    prologue.push(format!("local {ns} = {vs}"));
                }

                None => self.copy(ps, self.byte_end(p.name)),
            }

            cursor = self.byte_end(p.name);

            if p.ty.is_none()
                && self.text_of(p.name) == "self"
                && let Some(target) = self.self_type.clone()
            {
                self.generate(cursor, &format!(": {target}"));
            }

            if let Some(t) = p.ty {
                if bounds.is_empty() {
                    self.copy(cursor, self.byte_end(t));
                } else {
                    let ts = self.byte_start(t);
                    self.copy(cursor, ts);
                    let text = self.copy_type_to_string(t);
                    self.generate(ts, &apply_bounds(&text, &bounds));
                }

                cursor = self.byte_end(t);

                if p.default.is_some() && !self.text_of(t).trim_end().ends_with('?') {
                    self.generate(cursor, "?");
                }
            }

            if let Some(default) = &p.default {
                let name = self.text_of(p.name).to_string();
                let value = self.render_to_string(default);
                let ty =
                    p.ty.map(|t| format!(": {}", self.text_of(t)))
                        .unwrap_or_default();
                prologue.push(format!(
                    "local {name}{ty} = if {name} == nil then {value} else {name}"
                ));
                // ` = default` disappears from the parameter list.
                cursor = self.byte_end(default.span());
            }
        }

        // 3. The close paren and the return type, with `->` as `:` and an
        //    async payload type wrapped in Future.
        let close = self.toks[self.params_close_tok(body) as usize];
        self.copy(cursor, close.end);
        cursor = close.end;

        if let Some(arrow) = body.ret_arrow {
            let ae = self.byte_end(arrow);
            // `) -> T` becomes `): T`: the space before the arrow goes.
            self.generate(cursor, ":");
            cursor = ae;
        }

        if let Some(rt) = body.ret_type {
            let (rs, re) = (self.byte_start(rt), self.byte_end(rt));
            self.copy(cursor, rs);

            if body.is_async.is_some() {
                let std = self.std();
                self.generate(rs, &format!("{std}.Future<"));
                self.copy(rs, re);
                self.generate(re, ">");
            } else {
                self.copy(rs, re);
            }

            cursor = re;
        } else if body.is_async.is_some() && !returns_value(&body.block) {
            // An async body that returns nothing resolves to nothing: the
            // checker would infer `Future<unknown>` from the wrapper.
            // `()` is no type argument, so nil stands in; the editor
            // reads it back as `Future<()>`.
            let std = self.std();
            self.generate(cursor, &format!(": {std}.Future<nil>"));
        }

        // 4. The prologue and the async wrapper, on the header line.
        let has_vararg = body.params.iter().any(|p| p.is_vararg);
        let mut lead = String::new();

        if let Some(p) = self.self_prologue.take() {
            lead.push(' ');
            lead.push_str(&p);
        }

        for p in &prologue {
            lead.push(' ');
            lead.push_str(p);
        }

        // A body that returns nothing resolves to nil: the wrapper alone
        // would infer `Future<unknown>`, which the header's `Future<nil>`
        // refuses.
        let to_nil =
            body.is_async.is_some() && body.ret_type.is_none() && !returns_value(&body.block);

        if body.is_async.is_some() {
            let std = self.std();
            lead.push_str(&format!(
                " return {}{std}.future(function({})",
                if to_nil { "(" } else { "" },
                if has_vararg { "..." } else { "" }
            ));
        }

        if !lead.is_empty() {
            self.generate(cursor, &lead);
        }

        // 5. The body, its trailing trivia, and the close.
        let body_start = self.block_start_or(&body.block, end_tok.start);
        self.copy(cursor, body_start);
        self.function_block(body);
        let after = self.block_end_or(&body.block, body_start);
        self.copy(after, end_tok.start);

        if body.is_async.is_some() {
            let std = self.std();
            let close = if has_vararg { "end, ...)" } else { "end)" };
            let text = if to_nil {
                format!("{close} :: any) :: {std}.Future<nil> ")
            } else {
                format!("{close} ")
            };
            self.generate(end_tok.start, &text);
        }

        self.copy(end_tok.start, end_tok.end);
    }

    fn params_open_tok(&self, body: &FunctionBody) -> u32 {
        let mut i = body.span.start;

        while self.toks[i as usize].text(self.src) != "(" {
            i += 1;
        }

        i
    }

    /// The `)` that closes the parameter list: the last `)` before the
    /// return type, or before the body.
    fn params_close_tok(&self, body: &FunctionBody) -> u32 {
        let limit = match (body.ret_arrow, body.ret_type) {
            (Some(a), _) => a.start,

            (None, Some(t)) => t.start - 1,

            (None, None) => {
                if body.block.span.is_empty() {
                    body.span.end - 1
                } else {
                    body.block.span.start
                }
            }
        };
        let mut i = limit;

        while self.toks[i as usize].text(self.src) != ")" {
            i -= 1;
        }

        i
    }

    // --- loops -------------------------------------------------------------

    /// `for a, { x } in t where c do`: a filter and destructured variables.
    fn generic_for(&mut self, span: TokSpan, f: &GenericFor) {
        let start = self.byte_start(span);
        let mut cursor = start;
        let mut prologue: Vec<String> = Vec::new();
        let mut temp = 0;

        for v in &f.vars {
            let vs = self.byte_start(v.name);
            self.copy(cursor, vs);

            match &v.destructure {
                Some(d) => {
                    temp += 1;
                    let t = format!("_p{temp}");
                    self.generate(vs, &t);
                    let (ns, vals) = self.destructure_parts(d, &t);
                    prologue.push(format!("local {ns} = {vals}"));
                }

                None => self.copy(vs, self.byte_end(v.name)),
            }

            cursor = self.byte_end(v.name);

            if let Some(t) = v.ty {
                self.copy(cursor, self.byte_end(t));
                cursor = self.byte_end(t);
            }
        }

        // `in` and the iterator expressions.
        let last_expr = f
            .exprs
            .last()
            .map(|e| self.byte_end(e.span()))
            .unwrap_or(cursor);
        let children: Vec<Child<'_>> = f.exprs.iter().map(Child::Expr).collect();
        self.stitch_between(cursor, last_expr, &children);
        cursor = last_expr;

        // `do`, with the filter turned into a guard after it.
        let do_tok = self.find_tok_after(
            f.exprs.last().map(|e| e.span().end).unwrap_or(span.start),
            "do",
        );
        let do_start = self.toks[do_tok as usize].start;
        let do_end = self.toks[do_tok as usize].end;

        // The destructure prologue comes first, so the filter can read the
        // names it binds.
        match &f.filter {
            Some(c) => {
                let cond = self.render_to_string(c);
                let where_tok = self.toks[c.span().start as usize - 1];
                self.copy(cursor, where_tok.start);
                self.copy(do_start, do_end);

                for p in &prologue {
                    self.generate(do_end, &format!(" {p}"));
                }

                self.generate(do_end, &format!(" if not ({cond}) then continue end"));
            }

            None => {
                self.copy(cursor, do_end);

                for p in &prologue {
                    self.generate(do_end, &format!(" {p}"));
                }
            }
        }

        cursor = do_end;
        self.scopes.push(HashSet::new());

        for v in &f.vars {
            self.declare_binding(v);
        }

        let body_start = self.block_start_or(&f.block, cursor);
        self.copy(cursor, body_start);
        self.block(&f.block);
        let after = self.block_end_or(&f.block, body_start);
        self.scopes.pop();
        let end_tok = self.toks[span.end as usize - 1];
        self.copy(after, end_tok.start);
        self.copy(end_tok.start, end_tok.end);
    }

    fn find_tok_after(&self, from: u32, text: &str) -> u32 {
        let mut i = from;

        while self.toks[i as usize].text(self.src) != text {
            i += 1;
        }

        i
    }
}

struct ChainParts {
    guard: Option<String>,
    inner: String,
}

/// One step of a postfix chain, without its optionality.
enum Step<'a> {
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

enum Link<'a> {
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

/// Reports if a chain holds any link that is not plain Luau, or a base
/// that Luau cannot index directly.
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

/// The condition expressions a statement evaluates more than once, or
/// after other statements ran: `while`, `repeat`, and every `elseif`.
fn reevaluated_conditions(s: &Stmt) -> Vec<*const Expr> {
    match s {
        Stmt::While(w) => match &w.cond {
            Cond::Expr(e) => vec![e as *const Expr],

            Cond::Local { .. } => Vec::new(),
        },

        Stmt::Repeat(r) => vec![&r.cond as *const Expr],

        Stmt::If(i) => i
            .branches
            .iter()
            .skip(1)
            .filter_map(|(c, _)| match c {
                Cond::Expr(e) => Some(e as *const Expr),

                Cond::Local { .. } => None,
            })
            .collect(),

        _ => Vec::new(),
    }
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

        Stmt::ExportDefault { value, .. } => vec![Child::Expr(value)],

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

        Child::Block(b) => b.stmts.iter().any(stmt_needs_desugar),

        Child::Function(f) => f.block.stmts.iter().any(stmt_needs_desugar),
    })
}

/// Whether the text calls `name(` as a plain name: the struct call the
/// renderer reports. A member `x.Name(` or a longer word is not it.
fn struct_called(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before = start.checked_sub(1).map(|b| bytes[b]);
        let word_before = before
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b':');
        let after = text[end..].trim_start();

        if !word_before && after.starts_with('(') {
            return true;
        }

        from = end;
    }

    false
}

/// A module path the analyzer can follow: a string, or a chain of names
/// and fields such as `script.Parent.Module`.
fn is_static_module(e: &Expr) -> bool {
    if matches!(e, Expr::String(_)) {
        return true;
    }

    let (base, links) = flatten(e);

    matches!(base, Expr::Name(_))
        && links
            .iter()
            .all(|l| matches!(l, Link::Plain(Step::Field(_))))
}

/// Whether the text has `name {` as a plain name: the fields form of a
/// struct, which the renderer checks against a written constructor.
fn struct_braced(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before = start.checked_sub(1).map(|b| bytes[b]);
        let word_before = before
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b':');
        let after = text[end..].trim_start();

        if !word_before && after.starts_with('{') {
            return true;
        }

        from = end;
    }

    false
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

/// Splits a generic list `<A, B>` at its top-level commas.
fn split_generics(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches('<').trim_end_matches('>');
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();

    for c in inner.chars() {
        match c {
            '<' | '(' | '{' => depth += 1,

            '>' | ')' | '}' => depth -= 1,

            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();

                continue;
            }

            _ => {}
        }

        cur.push(c);
    }

    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }

    parts
}

/// The bounds in a generic list: `<T: Shape, U>` gives `T -> Shape`.
fn generic_bounds(text: &str) -> Vec<(String, String)> {
    split_generics(text)
        .into_iter()
        .filter_map(|item| {
            let (name, bound) = item.split_once(':')?;

            Some((name.trim().to_string(), bound.trim().to_string()))
        })
        .collect()
}

/// A generic list without its bounds: `<T: Shape, U>` gives `<T, U>`.
fn strip_bounds(text: &str) -> String {
    let names: Vec<String> = split_generics(text)
        .into_iter()
        .map(|item| match item.split_once(':') {
            Some((name, _)) => name.trim().to_string(),

            None => item,
        })
        .collect();

    format!("<{}>", names.join(", "))
}

/// Rewrites each bounded generic name in a type to `(T & Bound)`.
fn apply_bounds(ty: &str, bounds: &[(String, String)]) -> String {
    let mut out = String::with_capacity(ty.len() + 16);
    let bytes = ty.as_bytes();
    let mut i = 0;
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    // Depth inside `{ }` or `< >`: a bound applies to the bare parameter
    // only. Under a table or a generic, `{ T & Bound }` is invariant and
    // no concrete argument would satisfy it.
    let mut depth = 0i32;

    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'<' => depth += 1,
            b'}' => depth -= 1,
            // `>` closes a generic; the one in `->` does not.
            b'>' if i > 0 && bytes[i - 1] != b'-' => depth -= 1,
            _ => {}
        }

        if depth <= 0 && is_word(bytes[i]) && (i == 0 || !is_word(bytes[i - 1])) {
            let start = i;

            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }

            let word = &ty[start..i];
            // A field name `T:` or a member `X.T` is not the generic.
            let is_field =
                bytes.get(i).is_some_and(|c| *c == b':') || (start > 0 && bytes[start - 1] == b'.');

            match bounds.iter().find(|(n, _)| n == word) {
                Some((_, bound)) if !is_field => out.push_str(&format!("({word} & {bound})")),

                _ => out.push_str(word),
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    out
}

/// A `<<A, B>>` list with every argument in Luau's own spelling. The
/// annotation the type edits rewrite never reaches here, so the list
/// carries the source text and needs the same rewrite.
fn type_args_text(text: &str) -> String {
    let Some(inner) = text.strip_prefix("<<").and_then(|t| t.strip_suffix(">>")) else {
        return text.to_string();
    };
    let parts: Vec<String> = split_top_level(inner, ',')
        .iter()
        .map(|p| array_types(p))
        .collect();

    format!("<<{}>>", parts.join(", "))
}

/// One type with `T[]` written as `Array<T>`, at every depth. The
/// bracket form is Alloy's own; Luau reads the named form alone.
fn array_types(text: &str) -> String {
    let text = text.trim();

    if let Some(inner) = text.strip_suffix('?') {
        return format!("{}?", array_types(inner));
    }

    if let Some(inner) = text.strip_suffix("[]") {
        return format!("Array<{}>", array_types(inner));
    }

    // `Name<A, B>`: each argument takes the same rewrite.
    if let Some(open) = text.find('<')
        && text.ends_with('>')
        && open > 0
    {
        let parts: Vec<String> = split_top_level(&text[open + 1..text.len() - 1], ',')
            .iter()
            .map(|p| array_types(p))
            .collect();

        return format!("{}<{}>", &text[..open], parts.join(", "));
    }

    text.to_string()
}

/// `<<A, B>>` as one type pack, `<<(A, B)>>`. Text already wrapped, or
/// empty, comes back unchanged.
fn pack_type_args(text: &str) -> String {
    let Some(inner) = text
        .strip_prefix("<<")
        .and_then(|t| t.strip_suffix(">>"))
        .map(str::trim)
    else {
        return text.to_string();
    };

    if inner.is_empty() || inner.starts_with('(') {
        return text.to_string();
    }

    format!("<<({inner})>>")
}

/// The targets a built-in attribute takes, or `None` when the name is
/// not one. The list mirrors `builtin_attribute_targets` in alloy-lsp.
fn builtin_attr_targets(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "derive" | "sealed" => &["struct", "enum"],

        "cfg" => &["function", "local"],

        "test" | "native" | "checked" | "deprecated" | "inline" | "noinline" => &["function"],

        "unreliable" | "ratelimit" | "timeout" | "validate" | "immediate" => &["remote"],

        "u8" | "u16" | "u32" | "i8" | "i16" | "i32" | "f32" => &["param", "field"],

        "rename" | "skip" => &["field"],

        _ => return None,
    })
}

/// The type name of a literal argument, for an attribute's parameter
/// type. Anything else reads `None`: only a literal checks here.
fn literal_kind(e: &Expr) -> Option<&'static str> {
    match e {
        Expr::Number(_) => Some("number"),
        Expr::String(_) => Some("string"),
        Expr::True(_) | Expr::False(_) => Some("boolean"),
        _ => None,
    }
}

/// Which side of a remote a file sees, from its name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Client,
    Server,
}

/// The side a file name declares: `ui.client.aly` is the client, and
/// `main.server.aly` is the server. Any other name is shared.
fn file_side(file: &str) -> Option<Side> {
    let stem = file
        .strip_suffix(".aly")
        .or_else(|| file.strip_suffix(".alx"))
        .unwrap_or(file);

    if stem.ends_with(".client") {
        Some(Side::Client)
    } else if stem.ends_with(".server") {
        Some(Side::Server)
    } else {
        None
    }
}

/// A source slice with every run of whitespace as one space, so it fits
/// on the line the generated text sits on.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Backticked names joined with commas and a final `and`.
fn list_names(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();

    match quoted.len() {
        0 => String::new(),
        1 => quoted[0].clone(),
        n => format!("{} and {}", quoted[..n - 1].join(", "), quoted[n - 1]),
    }
}

/// Why a parameter type cannot cross a remote, or `None` when it can.
/// Functions and threads never serialize; a `Future` or `Signal` holds
/// both.
/// The parts of a type text at a separator of depth zero: the members
/// of `{ a: T, b: { c: U } }` at its commas.
fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;

    for (i, c) in text.char_indices() {
        match c {
            '{' | '(' | '<' | '[' => depth += 1,
            '}' | ')' | '>' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&text[from..i]);
                from = i + c.len_utf8();
            }
            _ => {}
        }
    }

    parts.push(&text[from..]);

    parts
}

/// The length of the bracket group that opens at the start of `text`,
/// when it closes inside the text.
fn group_len(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in text.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;

            if depth == 0 {
                return Some(i + c.len_utf8());
            }
        }
    }

    None
}

/// The part of a remote parameter that cannot cross the wire: the field
/// path that holds it when the type is a record, the type, and why.
fn wire_offender(ty: &str) -> Option<(Option<String>, String, &'static str)> {
    let mut trimmed = ty.trim();

    while let Some(t) = trimmed
        .strip_suffix('?')
        .or_else(|| trimmed.strip_suffix("[]"))
    {
        trimmed = t.trim();
    }

    if let Some(inner) = trimmed.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        for part in split_top_level(inner, ',') {
            let part = part.trim();
            let Some((name, value)) = part.split_once(':') else {
                continue;
            };
            let Some((deeper, bad, why)) = wire_offender(value) else {
                continue;
            };
            let name = name
                .trim()
                .strip_prefix("read ")
                .or_else(|| name.trim().strip_prefix("write "))
                .unwrap_or(name.trim())
                .trim();
            let path = match deeper {
                Some(d) => format!("{name}.{d}"),

                None => name.to_string(),
            };

            return Some((Some(path), bad, why));
        }

        return None;
    }

    not_wire_type(trimmed).map(|why| (None, trimmed.to_string(), why))
}

fn not_wire_type(ty: &str) -> Option<&'static str> {
    if ty.contains("->") {
        return Some("is a function type");
    }

    let words = ty.split(|c: char| !c.is_alphanumeric() && c != '_');

    for w in words {
        match w {
            "thread" => return Some("is a coroutine"),
            "Future" => return Some("is a `Future`, which holds a coroutine"),
            "Signal" => return Some("is a `Signal`, which holds functions"),
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use crate::EmitOptions;

    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    fn lint_names(src: &str) -> Vec<&'static str> {
        crate::compile(src)
            .unwrap()
            .lints
            .iter()
            .map(|l| l.name)
            .collect()
    }

    #[test]
    fn a_payload_enum_alias_carries_the_metatable() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.check.contains(
                "type Shape = typeof(setmetatable({} :: { tag: \"Circle\", _1: number }, Shape))"
            ),
            "{}",
            out.check
        );
    }

    #[test]
    fn a_match_arm_binds_the_payload_the_variant_carries() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\nlocal s = Shape.Circle(1)\nlocal n = match s with\n    case Circle(r, extra) then r\n    case Rect(w) then w\nend\nprint(n)\n";
        let got = messages(src);
        assert!(
            got.iter()
                .any(|m| m == "the variant `Circle` carries 1 value, the arm binds 2"),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|m| m == "the variant `Rect` carries 2 values, the arm binds 1"),
            "{got:?}"
        );
    }

    #[test]
    fn a_match_arm_names_a_variant_the_enum_has() {
        let src = "enum Msg as\n    Join(number)\n    Chat(number, string)\nend\nlocal m = Msg.Join(1)\nlocal t = match m with\n    case Join(p) then \"j\"\n    case Chat(p, s) then \"c\"\n    case Quit(p) then \"q\"\nend\nprint(t)\n";
        let got = messages(src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "`Msg` has no variant `Quit`; its variants are `Join` and `Chat`"
        );
    }

    #[test]
    fn an_enum_member_that_is_no_variant_reports() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\n    Empty\nend\nlocal nope = Shape.Triangle(1)\nprint(nope)\n";
        let got = messages(src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "`Shape` has no variant `Triangle`; its variants are `Circle`, `Rect` and `Empty`"
        );
    }

    #[test]
    fn an_enum_method_and_the_is_test_are_not_variants() {
        let src = "enum Shape as\n    Circle(number)\nend\nimpl Shape\n    function area(self): number\n        return 1\n    end\nend\nprint(Shape.is(1), Shape.area)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn try_needs_a_function_that_returns_result() {
        let bad = "local function g(): number\n    local v = try Ok(1)\n    return v + 1\nend\nprint(g)\n";
        assert!(
            messages(bad)
                .iter()
                .any(|m| m.starts_with("`try` works only inside a function that returns Result")),
            "{:?}",
            messages(bad)
        );

        let none = "local function g()\n    local v = try Ok(1)\n    return v\nend\nprint(g)\n";
        assert!(
            messages(none)
                .iter()
                .any(|m| m.starts_with("`try` works only inside a function that returns Result")),
            "{:?}",
            messages(none)
        );

        let fine = "local function g(): Result<number, string>\n    local v = try Ok(1)\n    return Ok(v + 1)\nend\nprint(g)\n";
        assert!(messages(fine).is_empty(), "{:?}", messages(fine));

        let alias = "type R = Result<number, string>\nlocal function g(): R\n    local v = try Ok(1)\n    return Ok(v + 1)\nend\nprint(g)\n";
        assert!(messages(alias).is_empty(), "{:?}", messages(alias));
    }

    #[test]
    fn signal_new_takes_its_type_arguments_as_a_pack() {
        let out = crate::compile("local s = Signal.new<<Player, number>>()\nprint(s)\n").unwrap();
        assert!(
            out.check
                .contains("__alloy.Signal.new<<(Player, number)>>()"),
            "{}",
            out.check
        );
    }

    #[test]
    fn an_attribute_checks_its_name_its_target_and_its_arguments() {
        let unknown = "@bogus\nlocal function one() end\nprint(one)\n";
        assert!(
            messages(unknown)
                .iter()
                .any(|m| m.starts_with("no attribute named `bogus`")),
            "{:?}",
            messages(unknown)
        );

        let target = "@derive(Clone)\nlocal function four() end\nprint(four)\n";
        assert!(
            messages(target)
                .iter()
                .any(|m| m == "the attribute `derive` has no meaning on a function; it goes on `struct` and `enum`"),
            "{:?}",
            messages(target)
        );

        let both = "@inline\n@noinline\nlocal function seven() end\nprint(seven)\n";
        assert!(
            messages(both).iter().any(|m| m.contains("opposite things")),
            "{:?}",
            messages(both)
        );

        let count = "@deprecated(1, 2, 3)\nlocal function six() end\nprint(six)\n";
        assert!(
            messages(count)
                .iter()
                .any(|m| m == "the attribute `deprecated` takes one message, 3 given"),
            "{:?}",
            messages(count)
        );

        let wrong =
            "attribute tag(name: string) on struct\n@tag(5)\nstruct S as x: number end\nprint(S)\n";
        assert!(
            messages(wrong)
                .iter()
                .any(|m| m == "the attribute `tag` takes string for `name`, number given"),
            "{:?}",
            messages(wrong)
        );

        let fine = "attribute range(min: number, max: number) on field\nstruct S as\n    @range(0, 1)\n    x: number\nend\nprint(S)\n";
        assert!(messages(fine).is_empty(), "{:?}", messages(fine));
    }

    #[test]
    fn inline_and_noinline_never_reach_the_emit() {
        let out = crate::compile(
            "@inline\nlocal function small(): number\n    return 1\nend\nprint(small())\n",
        )
        .unwrap();
        assert!(!out.ship.contains("@inline"), "{}", out.ship);
        assert!(!out.check.contains("@inline"), "{}", out.check);
    }

    #[test]
    fn a_struct_declares_each_field_once() {
        let src = "struct S as\n    a: number\n    a: string\nend\nprint(S)\n";
        assert!(
            messages(src)
                .iter()
                .any(|m| m == "`S` declares the field `a` twice"),
            "{:?}",
            messages(src)
        );
    }

    #[test]
    fn await_on_a_literal_reports() {
        let src = "local bad = await 5\nprint(bad)\n";
        assert!(
            messages(src)
                .iter()
                .any(|m| m == "`await` takes a Future or an awaitable, not a number"),
            "{:?}",
            messages(src)
        );
    }

    #[test]
    fn derive_debug_writes_a_debug_method() {
        let out =
            crate::compile("@derive(Debug)\nstruct V as\n    x: number\nend\nprint(V)\n").unwrap();
        assert!(out.ship.contains("function V.debug(self)"), "{}", out.ship);
        assert!(out.ship.contains("V.__tostring = "), "{}", out.ship);
    }

    #[test]
    fn derive_eq_and_partial_eq_write_one_metamethod() {
        let out =
            crate::compile("@derive(Eq, PartialEq)\nstruct V as\n    x: number\nend\nprint(V)\n")
                .unwrap();
        assert_eq!(out.ship.matches("V.__eq = ").count(), 1, "{}", out.ship);
    }

    #[test]
    fn an_unknown_derive_lands_on_the_derive_name() {
        let src = "@derive(Sparkle)\nstruct A as\n    x: number\nend\nprint(A)\n";
        let out = crate::compile(src).unwrap();
        assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
        assert_eq!(
            &src[out.diagnostics[0].start as usize..out.diagnostics[0].end as usize],
            "Sparkle"
        );
    }

    #[test]
    fn an_exhaustive_match_statement_closes_its_chain() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\nlocal function area(s: Shape): number\n    match s with\n        case Circle(r) then\n            return r\n        case Rect(w, h) then\n            return w * h\n    end\nend\nprint(area)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship
                .contains("else error(\"match: no arm covers this value\", 2)"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_spread_keeps_the_types_of_its_parts() {
        let out = crate::compile(
            "local base = { x = 1, y = 2 }\nlocal merged = { ...base, z = 3 }\nprint(merged)\n",
        )
        .unwrap();
        assert!(
            out.check.contains(":: typeof(base) & typeof({ z = 3 })"),
            "{}",
            out.check
        );
    }

    #[test]
    fn a_positional_entry_after_a_spread_reports() {
        let src = "local base = { x = 1 }\nlocal t = { ...base, 7 }\nprint(t)\n";
        assert!(
            messages(src)
                .iter()
                .any(|m| m.starts_with("a positional entry after a spread")),
            "{:?}",
            messages(src)
        );
    }

    #[test]
    fn a_wire_message_names_the_field_that_holds_the_function() {
        let src = "remote Deep(payload: { name: string, cb: (number) -> () }) from client\n";
        assert!(
            messages(src).iter().any(|m| m
                == "remote `Deep`: parameter `payload` has field `cb` of type `(number) -> ()`, which is a function type; a remote carries only data"),
            "{:?}",
            messages(src)
        );
    }

    #[test]
    fn an_unreachable_default_fires_on_a_match_expression() {
        let src = "enum Color as Red, Green, Blue end\nlocal function full(c: Color): string\n    return match c with\n        case Color.Red then \"r\"\n        case Color.Green then \"g\"\n        case Color.Blue then \"b\"\n        default \"?\"\n    end\nend\nprint(full)\n";
        assert!(
            lint_names(src).contains(&"unreachable_default"),
            "{:?}",
            lint_names(src)
        );
    }

    #[test]
    fn a_generic_struct_types_its_constructor() {
        let out = crate::compile(
            "struct Box<T> as\n    value: T\n    count: number = 1\nend\nlocal b = new Box<<number>> { value = 5 }\nprint(b)\n",
        )
        .unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("function Box.__new<T>(f: { value: T, count: number? }): Box<T>"),
            "{}",
            out.check
        );
    }

    #[test]
    fn an_impl_may_name_the_structs_parameters() {
        let src = "struct Box<T> as\n    value: T\nend\nimpl Box<T>\n    function get(self): T\n        return self.value\n    end\nend\nprint(Box)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("function Box.get<T>(self: Box<T>): T"),
            "{}",
            out.check
        );
        // The header has no Luau form and never reaches the output.
        assert!(!out.ship.contains("impl"), "{}", out.ship);
    }

    #[test]
    fn a_remote_shows_one_side_in_a_side_named_file() {
        let src = "export remote Damage(target: Player, amount: number) from client\n";
        let client = EmitOptions {
            check: true,
            file_name: "src/ui.client.aly".to_string(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &client).unwrap();
        assert!(out.check.contains("fire: "), "{}", out.check);
        assert!(!out.check.contains("on: "), "{}", out.check);

        let server = EmitOptions {
            check: true,
            file_name: "src/main.server.aly".to_string(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &server).unwrap();
        assert!(out.check.contains("on: "), "{}", out.check);
        assert!(!out.check.contains("fire: "), "{}", out.check);

        // A shared module branches on `RunService` and sees both.
        let shared = EmitOptions {
            check: true,
            file_name: "src/remotes.aly".to_string(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &shared).unwrap();
        assert!(out.check.contains("fire: "), "{}", out.check);
        assert!(out.check.contains("on: "), "{}", out.check);
    }

    #[test]
    fn a_type_argument_list_writes_arrays_by_name() {
        // `T[]` is Alloy's spelling; a `<<...>>` list is read as Luau.
        let src = "local g: HashMap<string, number[]> = HashMap.new()\nlocal h: Array<number[][]> = Array.new()\nlocal i: Array<HashMap<string, number[]>> = Array.new()\nlocal j = new Array<<number[]>>()\nprint(g, h, i, j)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("HashMap.new<<string, Array<number>>>()"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("Array.new<<Array<Array<number>>>>()"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("Array.new<<HashMap<string, Array<number>>>>()"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("Array.new<<Array<number>>>()"),
            "{}",
            out.check
        );

        // The annotation may use the bracket form as well.
        let out =
            crate::compile("local b: HashMap<string, number>[] = Array.new()\nprint(b)\n").unwrap();
        assert!(
            out.check.contains("Array.new<<HashMap<string, number>>>()"),
            "{}",
            out.check
        );
    }

    #[test]
    fn a_method_insert_maps_back_to_the_method_name() {
        let src = "type Alias = { z: number }\nimpl Alias\n    function area(self): number\n        return self.z\n    end\nend\nprint(Alias)\n";
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
