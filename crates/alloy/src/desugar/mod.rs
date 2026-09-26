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
use alloy_syntax::lexer::{Tok, TokKind};

use crate::render::{NewlineInGenerated, Renderer, SpanMap};

pub(crate) mod attributes;
mod awaits;
pub(crate) mod contracts;
mod enums;
mod expressions;
mod macros;
mod modules;
pub(crate) mod namespaces;
mod remotes;
pub mod statements;
mod structs;
mod types;

use macros::MacroRef;
use namespaces::{NamespaceInfo, NsFrame, NsHoist};
pub(crate) use remotes::WIRE_WIDTHS;
pub(crate) use types::{group_len, qualify_names, split_top_level, strip_bounds};

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
    /// The emitted file's path, in the space its `require` paths are
    /// written in: `build/init.luau`, `build/deep/x.luau`. Luau reads
    /// `x/init.luau` as the module `x`, so a relative path in that file
    /// names a file beside `x`; the emit writes the path from there.
    /// Empty when the compile has no output path, and a relative path
    /// then stays as the source wrote it.
    pub module_rel: String,
    /// Each relative spec that leaves the file's mount, with the
    /// `@game/...` place its `require` writes. See
    /// `crate::project::mount_requires`.
    pub mount_requires: Vec<(String, String)>,
    /// The side the file's place in the game gives it, for a name with
    /// no `.server` or `.client`. See `crate::project::place_side`.
    pub mount_side: Option<crate::directives::Side>,
    /// The string passed to `require` for the runtime.
    pub std_require: String,
    /// The ship artifact's runtime require when it differs: under a
    /// mount it is the instance path, while the check artifact and the
    /// tests keep the file path.
    pub ship_std_require: Option<String>,
    /// A `.d.aly`: declarations only, no runtime tables, no std require.
    pub definitions: bool,
    /// Macros visible to an expansion, as source: a nested compile of a
    /// macro body sees the macros of the file it came from.
    pub macros: Vec<MacroSource>,
    /// The structs of the whole project, for the wire layout of a
    /// remote that carries one from another file.
    pub shapes: Vec<StructShape>,
    /// The imports of each project module. A wire layout reads a type
    /// name through the imports of the file that writes it.
    pub wire_scopes: Vec<WireScope>,
    /// Render the check artifact: a call to an extension method on a
    /// foreign type stays as written, and `self` in such an impl carries
    /// the target type, so the analyzer types both. The ship artifact
    /// routes the call through the dispatcher instead.
    pub check: bool,
    /// Extensions declared anywhere in the project, so a call by one of
    /// their names routes through the dispatcher in every file, not only
    /// in the file that declares the impl.
    pub extensions: Vec<crate::extensions::Extension>,
    /// `[std] globals`: the std names the source writes with no import.
    /// The default is `All`, so a compile with no project reads every
    /// name; a project's options carry its own setting, `None` unless
    /// it says otherwise.
    pub std_globals: crate::std_names::Globals,
    /// The `impl` blocks other files write on a struct or an enum this
    /// one declares. The check artifact declares each method on the
    /// class table, so the type carries what the runtime attaches. See
    /// `crate::extensions::struct_impls`.
    pub foreign_impls: Vec<crate::extensions::Extension>,
    /// Per struct of the project, the methods an `impl` of it in any
    /// file declares private, so `private_access` reports a call from a
    /// file that holds no impl of the struct. See
    /// `crate::extensions::project_impls`.
    pub foreign_privates: Vec<(String, Vec<String>)>,
    /// The limits of the complexity lints.
    pub thresholds: crate::lint::Thresholds,
    /// The byte ranges of the compiled text that an ingot's transform
    /// wrote. A block that opens there adds no depth to the complexity
    /// lints, since the author did not write it.
    pub generated: Vec<(u32, u32)>,
    /// Render the test artifact: a `@test` function stays in the output
    /// as a local, unregistered, for `alloy test` to call by name.
    pub tests: bool,
    /// The project configures a test runner, `[test] lest`. Without one
    /// `$expect` has nothing to call, so it reports.
    pub test_runner: bool,
    /// Per import spec, the type names the module exports, so a value
    /// import of a struct or an enum binds the type too. See
    /// `crate::modules::import_types`.
    pub import_types: Vec<(String, Vec<String>)>,
    /// The enums the imported modules declare, with their variants and
    /// payload counts. A `match` over an imported enum covers it.
    /// See `crate::modules::import_enums`.
    pub import_enums: Vec<(String, Vec<(String, usize)>)>,
    /// The remotes the imported modules declare, by the name this file
    /// binds, with whether the client and the server fire each one. See
    /// `crate::modules::import_remotes`.
    pub import_remotes: Vec<(String, (bool, bool))>,
    /// Per imported trait, the names of its default methods, so an
    /// `impl Trait for S` here flattens them in as a local trait's would.
    pub import_trait_defaults: Vec<(String, Vec<String>)>,
    /// Per imported trait, the methods it leaves to the impl: the name,
    /// the parameter count with `self` counted, and the return type.
    /// An `impl Trait for S` has to write each one. See
    /// `crate::modules::import_trait_methods`.
    pub import_trait_methods: crate::modules::TraitRequired,
    /// The imported async functions declared to return a `Result`: a
    /// `try await f()` on one is the Result itself. See
    /// `crate::modules::import_result_asyncs`.
    pub import_result_asyncs: Vec<String>,
    /// Per struct an imported module declares, its private field names,
    /// so `private_access` reports a read across a module boundary. See
    /// `crate::modules::import_privates`.
    pub import_privates: Vec<(String, Vec<String>)>,
    /// The functions the imported modules declare, with their parameter
    /// counts and deprecation notes, so `argument_count` and
    /// `deprecated_call` read a call across a module boundary. See
    /// `crate::modules::import_callables`.
    pub import_callables: Vec<(String, crate::flux::Callable)>,
    /// Per struct an imported module declares, each field with whether
    /// it carries a default, so `new Box { }` here reports the fields it
    /// leaves unset. See `crate::modules::import_struct_fields`.
    pub import_struct_fields: Vec<(String, Vec<(String, bool)>)>,
    /// Per struct an imported module declares, the type text of each
    /// field, with whether this file can write it the way the module
    /// does. A field's constructor takes the arguments the type names,
    /// as in the module. See `crate::modules::import_field_types`.
    pub import_field_types: Vec<(String, Vec<crate::declarations::FieldText>)>,
    /// Per struct an imported module declares that writes a
    /// constructor, the name of that `new` or `New`. A report of
    /// `Box(1)` reads it, so it names the constructor. See
    /// `crate::modules::import_struct_ctors`.
    pub import_struct_ctors: Vec<(String, String)>,
    /// Per import spec, the structs the module keeps a private view of,
    /// `Name__all`. An `impl` of one here types `self` as the view, so
    /// it reaches the struct's private members. See
    /// `crate::modules::import_private_views`.
    pub import_private_views: Vec<(String, Vec<String>)>,
    /// The specs that name a module Alloy does not compile: a `.luau`
    /// or `.lua` file, or a data file. Such a module has no export
    /// table, so `import X from` binds the value it returns, not
    /// `require(...).default`. See `crate::modules::plain_modules`.
    pub plain_modules: Vec<String>,
    /// Every name a `.d.aly` of the project declares. Such a name has
    /// no module behind it, so a check that asks whether a name exists
    /// has to read the list.
    pub ambient_names: Vec<String>,
    /// The `export attribute` declarations of the modules this file
    /// imports, by name. An attribute contract is checked where the
    /// attribute is used, so a use here needs the declaration there.
    /// See `crate::modules::import_attributes`.
    pub import_attributes: Vec<(String, AttrDecl)>,
    /// Each star import of an Alloy module: the local, the paths of the
    /// namespaces the module declares, and every name it exports.
    /// `import_attributes` lists the attributes of the module and of
    /// those namespaces in full, so `@M.tag` or `@M.Ns.tag` that names
    /// none of them reports. See `crate::modules::import_star_modules`.
    pub import_star_modules: Vec<(String, Vec<String>, Vec<String>)>,
    /// The private attributes of the namespaces the imported modules
    /// export, by the path this file writes, so a use of one reports
    /// that it is private. See `crate::modules::import_private_attributes`.
    pub import_private_attributes: Vec<String>,
    /// The enums the file around a macro expansion declares. A macro
    /// body compiles as a fragment of its own, and a `match` in it
    /// covers the enums of the file it lands in. See `compile_fragment`.
    pub macro_enums: Vec<(String, Vec<(String, usize)>)>,
    /// How many macro expansions are open above a fragment. A textual
    /// expansion of a macro that calls itself has no end, so a depth
    /// of 16 stops it. See `expand_macro`.
    pub macro_depth: usize,
    /// `[lint.naming]`: the case style of each kind of name, for the
    /// `naming_convention` lint.
    pub naming: crate::naming::Naming,
    /// The markup of an `.alx` file, for the component names.
    pub markup: crate::naming::Markup,
    /// The byte ranges of the lowered `.alx` text that hold an attribute
    /// value or a lone `{expr}` that sets `Text`, with the type it must
    /// have. The check artifact passes each one through `__alloy.prop`,
    /// cast to that type.
    pub attribute_types: Vec<(u32, u32, String)>,
    /// The check artifact goes to Luau's new solver, `[flux] new_solver`.
    /// A type that only one solver reads right picks its form by it.
    pub new_solver: bool,
}

/// One field of a struct or an interface, as the prescan keeps it.
#[derive(Debug, Clone)]
struct FieldType {
    name: String,
    ty: TokSpan,
    private: bool,
}

/// One `attribute name(params) on targets` the file declares: the
/// targets it takes, each parameter's name and type, and the contract
/// its `requires` clauses state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttrDecl {
    pub targets: Vec<String>,
    pub params: Vec<(String, Option<String>)>,
    /// One entry per parameter: the default's source text, or `None`
    /// for a parameter every use has to write.
    pub defaults: Vec<Option<String>>,
    /// The `requires` clauses, in the order the body writes them. Empty
    /// for a declaration that states no contract.
    pub requires: Vec<Require>,
}

/*
A member an attribute contract asks for that the declaration under the
attribute does not carry.

The report is the compiler's answer. This is the editor's: it says what
to write and where, so the quick fix inserts a member that compiles and
the completion offers its name.
*/
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractGap {
    /// The attribute the contract belongs to, with no sigil.
    pub attr: String,
    /// The byte range of the attribute, where the report sits.
    pub start: u32,
    pub end: u32,
    pub member: String,
    /// `function` or `field`.
    pub kind: String,
    /// `private`, `public`, or empty when the clause takes either.
    pub visibility: String,
    /// A function's parameter list, `(self)`, or a field's type. Empty
    /// when the clause states no shape.
    pub shape: String,
    /// The byte offset of the `end` that closes the declaration. The
    /// member goes on its own line in front of it.
    pub insert_at: u32,
    /// The column that `end` sits at, in bytes. A member of the body
    /// sits one indent further in.
    pub indent: u32,
}

pub use attributes::signature_ret_type;
pub use contracts::{element_type, is_string_union};
pub use statements::{names_a_future, pattern_type, signature_with_pattern_types};

/// One `requires` clause of an attribute contract, as the prescan keeps
/// it. The check reads this and the members of the thing the attribute
/// sits on; nothing here reaches the emit.
#[derive(Debug, Clone, PartialEq)]
pub struct Require {
    /// `Some(true)` for `private`, `Some(false)` for `public`, and None
    /// when the clause takes either.
    pub private: Option<bool>,
    /// `function` or `field`.
    pub kind: String,
    /// The member the clause asks for, or the parameter `each` reads.
    pub member: String,
    /// True when `member` names a parameter whose entries are the names.
    pub each: bool,
    /// A function's parameter list as the clause writes it, `(self)`, or
    /// a field's type. Empty when the clause states no shape.
    pub shape: String,
}

pub use structs::RENAME_STYLES;

/// A struct another file declares, for the wire layout of a remote
/// that carries it: each field with its type text and its width, and the
/// derives it takes, so an importer's own derives reach through it.
/// An enum rides the same list with its variants and no fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructShape {
    pub name: String,
    pub fields: Vec<WireField>,
    pub derives: Vec<String>,
    /// The declaring source, relative to the project's `in` folder. It
    /// keys the table the runtime registers for the wire.
    pub module: String,
    /// Each variant with the type of each payload value; empty for a
    /// struct.
    pub variants: Vec<(String, Vec<String>)>,
}

impl StructShape {
    /// The key the declaring file registers the table under, and the
    /// layout of a file that cannot name the table.
    pub fn wire_key(&self) -> String {
        format!("{}:{}", self.module, self.name)
    }
}

/// The imports of one project module. A type name in the module means
/// the type these imports bind, so a same-named type elsewhere in the
/// project does not change the layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WireScope {
    /// The module, relative to the project's `in` folder, as in
    /// `StructShape::module`.
    pub module: String,
    /// Each name an import binds, with the module and the name there:
    /// `import { Inner as I }` binds `I` to `Inner`.
    pub names: Vec<(String, String, String)>,
    /// Each `import * as K`, with its module.
    pub stars: Vec<(String, String)>,
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
    /// The macro is not callable in this file. A private macro of an
    /// imported module travels with the module's exported macros, so an
    /// expansion of one can call it, and the import list cannot.
    pub hidden: bool,
    pub params: Vec<String>,
    /// The default of each parameter, as source text.
    pub defaults: Vec<Option<String>>,
    /// The names each pattern parameter binds, with the access that
    /// reads each one from the argument: `(x, ".x")`. See
    /// `pattern_accesses`. Empty for a parameter with a name.
    pub patterns: Vec<Vec<(String, String)>>,
    pub variadic: bool,
    pub body: String,
    pub tail: Option<String>,
}

/// The names a pattern parameter binds and the access that reads each
/// from the argument: `.field` for a table pattern, `[i]` for an array,
/// and for `...rest` a function the argument passes through.
pub(crate) fn pattern_accesses(
    p: &alloy_syntax::ast::Param,
    text: impl Fn(TokSpan) -> String,
) -> Vec<(String, String)> {
    match &p.destructure {
        Some(Destructure::Table(fields)) => {
            let named: Vec<String> = fields
                .iter()
                .filter(|f| !f.rest)
                .map(|f| text(f.field))
                .collect();

            fields
                .iter()
                .map(|f| match f.rest {
                    true => (text(f.field), statements::rest_copy(&named)),

                    false => (
                        text(f.rename.unwrap_or(f.field)),
                        format!(".{}", text(f.field)),
                    ),
                })
                .collect()
        }

        Some(Destructure::Array { items, .. }) => items
            .iter()
            .enumerate()
            .map(|(i, n)| (text(*n), format!("[{}]", i + 1)))
            .collect(),

        None => Vec::new(),
    }
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            wait_timeout: None,
            file_name: "<input>".to_string(),
            module_rel: String::new(),
            mount_requires: Vec::new(),
            mount_side: None,
            std_require: "@alloy".to_string(),
            ship_std_require: None,
            definitions: false,
            macros: Vec::new(),
            shapes: Vec::new(),
            wire_scopes: Vec::new(),
            check: false,
            extensions: Vec::new(),
            std_globals: crate::std_names::Globals::All,
            foreign_impls: Vec::new(),
            foreign_privates: Vec::new(),
            thresholds: crate::lint::Thresholds::default(),
            generated: Vec::new(),
            tests: false,
            test_runner: true,
            import_types: Vec::new(),
            import_enums: Vec::new(),
            import_remotes: Vec::new(),
            import_trait_defaults: Vec::new(),
            import_trait_methods: Vec::new(),
            import_result_asyncs: Vec::new(),
            import_privates: Vec::new(),
            import_callables: Vec::new(),
            import_struct_fields: Vec::new(),
            import_field_types: Vec::new(),
            import_struct_ctors: Vec::new(),
            import_private_views: Vec::new(),
            plain_modules: Vec::new(),
            ambient_names: Vec::new(),
            import_attributes: Vec::new(),
            import_star_modules: Vec::new(),
            import_private_attributes: Vec::new(),
            macro_enums: Vec::new(),
            macro_depth: 0,
            naming: crate::naming::Naming::default(),
            markup: crate::naming::Markup::default(),
            attribute_types: Vec::new(),
            new_solver: true,
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
    /// The source byte ranges of those blanks: a type-only import, a
    /// test.
    pub ship_dropped: Vec<(u32, u32)>,
    /// Whether the file required the std.
    pub uses_std: bool,
    /// Whether the file declares an extension on a foreign type, so the
    /// check artifact differs from the ship artifact.
    pub ext_used: bool,
    /// The `@test` functions: name and whether it is async.
    pub tests: Vec<(String, bool)>,
    /// The members an attribute contract asked for and did not find.
    pub contract_gaps: Vec<ContractGap>,
    /// The byte each member name starts at that an attribute contract
    /// asks for. The contract fixes the name, so the naming lint leaves
    /// that member alone, and reads another member of the same name.
    pub contract_names: HashSet<u32>,
}

/// The words a declaration may write between `global` and the name it
/// declares, for the report `global` draws.
const DECL_WORDS: &[&str] = &[
    "local",
    "const",
    "function",
    "async",
    "struct",
    "enum",
    "trait",
    "interface",
    "remote",
    "impl",
    "class",
    "open",
    "macro",
    "attribute",
    "namespace",
    "type",
    "export",
];

/// The std names that are ambient in Alloy source.
pub const AMBIENT: &[&str] = &[
    "Future",
    "Result",
    "Ok",
    "Err",
    "Array",
    "HashMap",
    "Set",
    "BitSet",
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
    "BitSet",
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
    "R15Character",
    "R6Character",
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

        TypeEdit::Negation { tildes, operand } => (
            toks[tildes.start as usize].start,
            u32::MAX - toks[operand.end as usize - 1].end,
        ),

        TypeEdit::TypeOf(span) => (toks[span.start as usize].start, 0),
    });

    let (std_imports, std_namespaces, std_aliases) = std_imports(src, toks, chunk);
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
        field_expected: HashMap::new(),
        field_casts: HashMap::new(),
        variant_casts: HashMap::new(),
        value_sink: None,
        for_header: 0,
        child_cast: None,
        require_arg: false,
        chain_target: false,
        expected_payload: None,
        result_asyncs: options.import_result_asyncs.iter().cloned().collect(),
        // Luau reads a reserved word as a key only in brackets.
        inserts: chunk
            .reserved_keys
            .iter()
            .map(|k| toks[k.start as usize])
            .flat_map(|t| [(t.start, "[\"".to_string()), (t.end, "\"]".to_string())])
            .collect(),
        return_at: None,
        struct_field_types: HashMap::new(),
        struct_wire: HashMap::new(),
        struct_at: HashMap::new(),
        temp_next: 0,
        taken_temps: toks
            .iter()
            .filter_map(|t| {
                let text = t.text(src).strip_prefix('_')?;
                let digits = text.trim_start_matches(['m', 'v', 'c', 'p']);

                (digits.len() + 1 >= text.len())
                    .then(|| digits.parse().ok())
                    .flatten()
            })
            .collect(),
        declared: Vec::new(),
        lazy: false,
        effects: false,
        reads: false,
        in_place: false,
        chain_anchor: 0,
        barrier: 0,
        type_edits: edits,
        scopes: vec![HashSet::new()],
        uses_std: false,
        top_scope: 1,
        // `ui.client.aly` sees the client half of a remote, and
        // `main.server.aly` the server half. A module under
        // `ServerScriptService` runs on the server alone.
        file_side: crate::directives::file_side(&options.file_name).or(options.mount_side),
        remote_sides: options.import_remotes.iter().cloned().collect(),
        remote_shadows: Vec::new(),
        remote_aliases: HashMap::new(),
        own_names: top_level_names(src, toks, chunk),
        exports: Vec::new(),
        has_default_export: false,
        enums: HashMap::from([(
            "Result".to_string(),
            vec![("Ok".to_string(), 1), ("Err".to_string(), 1)],
        )]),
        attr_decls: HashMap::new(),
        imported_names: HashSet::new(),
        import_renames: HashMap::new(),
        star_modules: HashSet::new(),
        star_specs: HashMap::new(),
        private_view_names: HashSet::new(),
        ret_types: Vec::new(),
        try_targets: Vec::new(),
        one_value: HashSet::new(),
        result_aliases: HashSet::new(),
        alias_values: HashMap::new(),
        fn_ret_types: HashMap::new(),
        plain_fns: HashSet::new(),
        binding_types: HashMap::new(),
        enum_decls: HashMap::new(),
        enum_payloads: HashMap::new(),
        impl_methods: HashMap::new(),
        contract_gaps: Vec::new(),
        contract_names: HashSet::new(),
        field_body: HashMap::new(),
        method_body: HashMap::new(),
        type_members: HashMap::new(),
        renames: Vec::new(),
        ship_blanks: Vec::new(),
        top_imports: Vec::new(),
        head_requires: Vec::new(),
        structs: HashSet::new(),
        hoisted: Vec::new(),
        hoisted_fns: Vec::new(),
        ns_hoisted: Vec::new(),
        struct_generics: HashMap::new(),
        structs_with_new: HashMap::new(),
        impl_target: None,
        impl_method: None,
        table_selfs: HashMap::new(),
        self_inject: None,
        declared_types: HashSet::new(),
        traits: HashMap::new(),
        self_dispatch: HashSet::new(),
        trait_required: HashMap::new(),
        struct_fields: HashMap::new(),
        generic_types: HashSet::new(),
        struct_methods: HashMap::new(),
        impl_generics: HashMap::new(),
        fn_bounds: HashMap::new(),
        method_bounds: HashMap::new(),
        impl_traits: HashMap::new(),
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
        macro_stmt: false,
        macro_followed: false,
        structs_with_to_string: HashSet::new(),
        serializable: HashSet::new(),
        deserializable: HashSet::new(),
        cloneable: HashSet::new(),
        equatable: HashSet::new(),
        defaultable: HashSet::new(),
        std_imports,
        std_namespaces,
        std_aliases,
        std_reported: HashSet::new(),
        private_types: HashSet::new(),
        self_prologue: None,
        not_constructible: HashMap::new(),
        mapped_used: Vec::new(),
        uses_neg: false,
        namespaces: HashMap::new(),
        member_names: HashMap::new(),
        ns_stack: Vec::new(),
        ns_export: false,
        ns_test: false,
        ns_force_local: false,
        export_listed: HashSet::new(),
        export_listed_bare: HashSet::new(),
        reexported_types: HashSet::new(),
        export_listed_types: HashSet::new(),
        file_types: HashMap::new(),
        imported_types: HashMap::new(),
        type_name_spans: chunk.type_names.clone(),
        non_value_spans: {
            let mut spans = Vec::new();
            stmts_non_value_spans(&chunk.block.stmts, &mut spans);

            spans
        },
    };

    for (name, decl) in &options.import_attributes {
        d.attr_decls.insert(name.clone(), decl.clone());
    }

    for m in options.macros.iter().filter(|m| !m.hidden) {
        d.macros.insert(
            m.name.clone(),
            MacroRef {
                params: m.params.clone(),
                defaults: m.defaults.clone(),
                patterns: m.patterns.clone(),
                variadic: m.variadic,
                body: m.body.clone(),
                tail: m.tail.clone(),
            },
        );
    }

    // `global` left the language; each one the file wrote reports.
    d.report_globals(src, toks, chunk);

    // `id<number>(5)` is valid Luau, two comparisons, and compiles as
    // that. The author most likely meant a call; see `angle_calls`.
    for (span, message) in &chunk.angle_calls {
        d.lints.push(Lint {
            name: "single_angle_call",
            start: toks[span.start as usize].start,
            end: toks[span.end as usize - 1].end,
            message: message.clone(),
            fix: None,
        });
    }

    // A namespace names its members before the prescan reads them: a
    // member renders under the namespace's prefix, and every table the
    // prescan fills is keyed by that rendered name.
    d.scan_namespaces(&chunk.block);
    d.check_namespaces(&chunk.block);
    d.check_duplicate_decls(&chunk.block);
    d.check_exports(&chunk.block);

    // Names that later statements route through, gathered up front.
    d.prescan(&chunk.block);
    d.scan_hoisted(&chunk.block);
    d.scan_ns_hoists(&chunk.block);
    d.scan_plain_tables(&chunk.block);
    d.scan_reduce_inserts(&chunk.block);
    d.note_remote_sides(&chunk.block);
    d.scan_static_checks(&chunk.block);
    d.check_await_spots(&chunk.block);

    if !d.std_namespaces.is_empty() {
        d.check_std_star_members(&chunk.block);
    }
    d.check_negations();

    // Leading trivia, the block, trailing trivia: the printer's shape. The
    // std require, when the file needs one, goes on the first line of
    // code. Luau reads a `--!strict` only above the first token, and a
    // plain comment may sit above the hot comments.
    let insert_at = first_code_line(src, toks) as u32;
    d.copy(0, insert_at);

    let mut side = Renderer::new(src);
    std::mem::swap(&mut d.r, &mut side);

    match toks.first() {
        Some(first) => {
            let first_start = first.start.max(insert_at);
            d.copy(insert_at, first_start);
            d.top_scope = d.scope_depth();
            d.block(&chunk.block);
            d.check_bound_calls(&chunk.block);
            d.check_identity_compares(&chunk.block);
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
        let line = runtime_prologue(options);
        prefix_len = line.len() as u32;
        d.generate(insert_at, &line);
    }

    // After `set_testing`, so a module an import in a test names loads
    // under the test flag too.
    if !d.head_requires.is_empty() {
        let line = d.head_requires.concat();
        d.generate(insert_at, &line);
    }

    // Luau's solver crashes on the negation of a union or an
    // intersection with a table in it, and the crash drops every check
    // of the file. Each member negates first, so a table member errors
    // the way a table does.
    if d.uses_neg {
        d.generate(
            insert_at,
            "type function __neg(t) local function each(u) if u:is(\"union\") or u:is(\"intersection\") then for _, c in u:components() do each(c) end else types.negationof(u) end end each(t) return types.negationof(t) end ",
        );
    }

    for kind in d.mapped_used.clone() {
        let line = format!("{} ", Desugar::mapped_type_function(kind));
        d.generate(insert_at, &line);
    }

    // A table a function reads above its declaration opens on the first
    // line, so no line count moves, and the declaration fills it. One
    // table from the first line on: the checker types every write to
    // it as one, where a forward `local Point` alone reads as nil.
    if !d.hoisted.is_empty() {
        let tables = vec!["{}"; d.hoisted.len()].join(", ");
        let line = format!("local {} = {tables} ", d.hoisted.join(", "));
        d.generate(insert_at, &line);
    }

    // A function reads the same way, and a bare `local f` is enough:
    // the checker types the slot from the `function f()` that fills
    // it, where `f = function` would leave it optional.
    if !d.hoisted_fns.is_empty() {
        let line = format!("local {} ", d.hoisted_fns.join(", "));
        d.generate(insert_at, &line);
    }

    let _ = prefix_len;
    // The export return follows the last statement; a blanked test at
    // the end of the file must not take it along.
    let return_start = d.return_at.map(|at| d.r.out_len() + at);
    // The prologue anchors on the first statement, so a blanked import
    // there must not take the runtime require along.
    let body_start = d.r.out_len();
    d.r.append(side);

    d.drop_test_only_imports();
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

        if start < body_start || return_start.is_some_and(|r| start >= r) {
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
        ship_dropped: blanks,
        uses_std: d.uses_std,
        ext_used: d.ext_hit,
        tests: d.test_names,
        contract_gaps: d.contract_gaps,
        contract_names: d.contract_names,
    }
}

/// The names one top-level statement binds.
pub fn bound_names(src: &str, toks: &[Tok], stmt: &Stmt) -> Vec<String> {
    let text = |span: TokSpan| span.text_or_empty(src, toks).to_string();

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
pub(crate) fn top_level_names(src: &str, toks: &[Tok], chunk: &Chunk) -> HashSet<String> {
    let text = |span: TokSpan| span.text_or_empty(src, toks).to_string();
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

/// The std names a file's imports bind under their own names, then the
/// locals its star imports of the std bind, then each alias of a std
/// name with the name. A name under an alias binds the alias, a local
/// the import writes.
fn std_imports(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
) -> (
    HashSet<String>,
    HashMap<String, String>,
    HashMap<String, String>,
) {
    use alloy_syntax::ast::ImportKind;

    let mut out = HashSet::new();
    let mut stars = HashMap::new();
    let mut aliases = HashMap::new();

    for i in imports_in(&chunk.block) {
        let spec = i.path.text(src, toks).trim_matches(['"', '\'']);

        let Some(module) = crate::std_names::module_of_spec(spec) else {
            continue;
        };

        if let ImportKind::Namespace(n, _) = &i.kind {
            stars.insert(n.text(src, toks).to_string(), module.to_string());
        }

        if let ImportKind::Named(specs)
        | ImportKind::TypeOnly(specs)
        | ImportKind::Both(_, specs)
        | ImportKind::Namespace(_, specs) = &i.kind
        {
            for s in specs {
                let name = s.name.text(src, toks).to_string();

                match s.alias {
                    Some(a) => {
                        aliases.insert(a.text(src, toks).to_string(), name);
                    }

                    None => {
                        out.insert(name);
                    }
                }
            }
        }
    }

    (out, stars, aliases)
}

/// Every `import` of a block at any depth, in source order. An import
/// resolves where it stands, so a function or a `do` block may hold one
/// for its own scope; each scan of what a file imports reads them all.
pub fn imports_in(block: &Block) -> Vec<&alloy_syntax::ast::Import> {
    fn walk<'a>(block: &'a Block, out: &mut Vec<&'a alloy_syntax::ast::Import>) {
        for stmt in &block.stmts {
            if let Stmt::Import(i) = stmt {
                out.push(i);

                continue;
            }

            for child in stmt_children(stmt) {
                match child {
                    Child::Block(b) => walk(b, out),

                    Child::Function(f) => walk(&f.block, out),

                    Child::Expr(e) => expr_imports(e, out),
                }
            }
        }
    }

    // A closure inside an expression holds a block of its own.
    fn expr_imports<'a>(e: &'a Expr, out: &mut Vec<&'a alloy_syntax::ast::Import>) {
        for child in expr_children(e) {
            match child {
                Child::Block(b) => walk(b, out),

                Child::Function(f) => walk(&f.block, out),

                Child::Expr(x) => expr_imports(x, out),
            }
        }
    }

    let mut out = Vec::new();
    walk(block, &mut out);
    out
}

/// Every name an `import` binds.
pub(crate) fn import_names(i: &alloy_syntax::ast::Import) -> Vec<TokSpan> {
    use alloy_syntax::ast::ImportKind;

    match &i.kind {
        ImportKind::Default(n) => vec![*n],

        ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => std::iter::once(*n)
            .chain(specs.iter().map(|s| s.alias.unwrap_or(s.name)))
            .collect(),

        ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
            specs.iter().map(|s| s.alias.unwrap_or(s.name)).collect()
        }
    }
}

/// The line that binds the runtime, on the first line of code. A spec
/// marks the run before the module's own code, so `@cfg(test)` holds
/// while the module loads too.
pub(crate) fn runtime_prologue(options: &EmitOptions) -> String {
    let testing = match options.tests {
        true => "__alloy.set_testing(true) ",

        false => "",
    };

    format!(
        "local __alloy = require({}) {testing}",
        luau_string(&options.std_require)
    )
}

/// The byte offset of the line that holds the first token: every
/// comment above it stays above the header.
fn first_code_line(src: &str, toks: &[Tok]) -> usize {
    toks.first().map_or(src.len(), |t| {
        src[..t.start as usize].rfind('\n').map_or(0, |i| i + 1)
    })
}

/// One thing a statement hoists in front of itself.
enum Hoist<'s> {
    /// `local _k = value`, or `_k = value` when the block declared it.
    Temp {
        index: u32,
        value: HoistValue<'s>,
        anchor: u32,
    },
    /// A whole statement, such as the early return of `try`. `exits`
    /// marks a `return`, which a closure cannot hold.
    Stmt {
        text: String,
        anchor: u32,
        exits: bool,
    },
    /// `local name = value`, always a new local: an import's module,
    /// whose type must not be another module's.
    Fresh {
        name: String,
        value: HoistValue<'s>,
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
    /// The field values of a `new S { ... }` under render, by address,
    /// with the arguments the field's declared type names: `visits =
    /// HashMap.new()` under `visits: HashMap<string, number>`.
    field_expected: HashMap<usize, (String, String)>,
    /// The field values of a `new S { ... }` of an imported struct, by
    /// address, with the type the check artifact casts each one to:
    /// `index<S, "rows">` for `rows = HashMap.new()`, where the field's
    /// type names a type this file cannot write.
    field_casts: HashMap<usize, String>,
    /// The items of a list literal that construct a variant, by
    /// address, with the enum the check artifact casts each one to.
    variant_casts: HashMap<usize, String>,
    /// The text a value-only `return` writes in front of its value: `s = `
    /// in an arm of `local s = match`, `return ` for a value block. None
    /// writes `return `.
    value_sink: Option<String>,
    /// Above zero while the check artifact renders a for-in header; see
    /// the `return` cast in `statements`.
    for_header: u32,
    /// The cast the check artifact puts on the child lookup under
    /// render. None emits the plain call, so luau-lsp types the child
    /// from the sourcemap. See `chain_parts`.
    child_cast: Option<String>,
    /// Set while the check artifact renders the argument of `require`.
    /// The child lookups of that chain lose their nil guards, since
    /// luau-lsp resolves a module from plain calls only.
    require_arg: bool,
    /// Set while an assignment target renders its object. A field
    /// follows the chain's last link, so a child there casts to `any`.
    chain_target: bool,
    /// `local f: Future<T> = async do ... end`: the payload type `T`,
    /// so the block's closure carries it. Without it the checker infers
    /// the closure's result, and an open result lands on `unknown`. An
    /// `async function` header reads its own return type the same way.
    expected_payload: Option<String>,
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
    /// Where each struct of this file starts. A remote's wire holds the
    /// struct's table, which a `local` below the remote has not made yet.
    struct_at: HashMap<String, u32>,
    /// The next temp index inside the statement under render.
    temp_next: u32,
    /// The temp numbers the source already names, `local _1 = 5` or a
    /// `_p1` parameter. The emit skips them, so a temp never shadows a
    /// user local.
    taken_temps: Vec<u32>,
    /// Per open block: the temp indices already declared in it, and in
    /// every block around it, since an inner block sees outer locals.
    declared: Vec<Vec<u32>>,
    /// Set around an operand that runs on some paths only: the right
    /// side of `and`, `or` and `??`, a later branch, a guard, a step past
    /// `?`, a loop condition. A hoist would run it on every path, and a
    /// loop condition once, so its hoists stay inside it.
    lazy: bool,
    /// The statement under render has called code. A later hoist would
    /// run in front of that call.
    effects: bool,
    /// The statement under render has read a name or a field. A later
    /// hoist that calls code would run in front of that read.
    reads: bool,
    /// Inside an operand that keeps its hoists. A chain then reads a
    /// plain prefix again instead of naming it, so it needs no closure.
    in_place: bool,
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
    /// The index of the scope the file's top level binds into. A local
    /// of any scope past it is a name of its own, and it shadows an
    /// ambient name the way it shadows anything else.
    top_scope: usize,
    /// The side this file sits on, from its name or its directive.
    file_side: Option<crate::directives::Side>,
    /// Each remote a name here reaches, declared or imported, with
    /// whether the client and the server fire it.
    remote_sides: HashMap<String, (bool, bool)>,
    /// The bindings of this file that share a name with a remote or an
    /// alias of one, with the tokens each one holds; see
    /// `naming::scoped_bindings`.
    remote_shadows: Vec<crate::naming::ScopedBinding>,
    /// Each remote a local holds, `const vote = Net.Vote`, keyed by the
    /// token that declares the local and the path through it.
    remote_aliases: HashMap<(usize, String), (bool, bool)>,
    /// Every name the top level of this file binds.
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
    /// `import { Box as B }`: the local name to the name the module
    /// declares. An index the import carries is keyed by the declared
    /// name, so a check of `new B { }` reads `Box` through this.
    import_renames: HashMap<String, String>,
    /// `import * as M`: the locals that stand for a whole module, so
    /// `new M.Box { }` names the struct `Box` the module declares.
    star_modules: HashSet<String>,
    /// `import * as M from "./m"`: each such local with the spec it
    /// names, so `export { M }` sends the module's types on.
    star_specs: HashMap<String, String>,
    /// Imported structs whose full view this file aliases as
    /// `Name__all`. An `impl` of one types `self` as the view.
    private_view_names: HashSet<String>,
    /// The declared return type of each function body under render,
    /// innermost last. `try` reads it: it compiles only inside a function
    /// that returns a Result.
    ret_types: Vec<Option<String>>,
    /// The return target of each nesting under render, innermost last:
    /// the `try do` block that owns it, or `None` for a function. `try`
    /// reads the last one to pick between the block's `fail` and a
    /// `return` from the function.
    try_targets: Vec<Option<TryTarget>>,
    /// Each `await` a `try do` block returns, by address, which the check
    /// artifact wraps in parens to keep its first value alone.
    one_value: HashSet<usize>,
    /// Type aliases the file declares whose value is a `Result`, so a
    /// function that returns one still takes `try`.
    result_aliases: HashSet<String>,
    /// The value each type alias of this file declares, as the source
    /// writes it. `is` reads through it: an alias is a second spelling
    /// of one type, not a type of its own.
    alias_values: HashMap<String, String>,
    /// The return type each top-level function of this file declares.
    /// `try` reads it: an operand whose type is no Result is an error
    /// at the `try`, not at the `return` under it.
    fn_ret_types: HashMap<String, String>,
    /// The top-level functions of this file that are not `async`. An
    /// `await` of a call to one reads its type in `fn_ret_types`.
    plain_fns: HashSet<String>,
    /// The type each annotated binding of this file declares, by name.
    /// `try await` reads it: a `Future<Result<...>>` settles with the
    /// Result itself. The map is flat, the way `fn_ret_types` is; a
    /// name reused with another type gives up its entry.
    binding_types: HashMap<String, String>,
    /// The enums this file declares, from the prescan, so a use before
    /// the declaration still checks. `enums` also holds `Result`, which
    /// the std owns and whose table carries more than its variants.
    enum_decls: HashMap<String, Vec<(String, usize)>>,
    /// The payload types of each enum this file declares, variant by
    /// variant, for the wire layout that restores a payload value.
    enum_payloads: HashMap<String, Vec<(String, Vec<String>)>>,
    /// Method and static names an `impl` block writes, by target. An enum
    /// member check reads it so a method call is not a missing variant.
    impl_methods: HashMap<String, HashSet<String>>,
    /// The members an attribute contract asked for and did not find, in
    /// source order. The report names each one; this is what the editor
    /// writes in.
    contract_gaps: Vec<ContractGap>,
    /// See [`Rendered::contract_names`].
    contract_names: HashSet<u32>,
    /// Where a member of a type goes, by the type's name: the span of
    /// the `struct` or `interface` that holds its fields, and the span of
    /// an `impl` that holds its methods. A contract on an `impl` can ask
    /// for a field, which belongs in the struct, so the quick fix writes
    /// each kind where the language puts it.
    field_body: HashMap<String, TokSpan>,
    method_body: HashMap<String, TokSpan>,
    /// Every member the file declares for a type, by the type's name:
    /// the fields of a `struct` or an `interface` and the methods of
    /// every `impl` over it. An attribute contract reads this, so a
    /// clause on an `impl` sees the struct's fields and a clause on the
    /// struct sees the impl's methods.
    type_members: HashMap<String, Vec<contracts::Member>>,
    /// Pattern bindings under substitution in expression arms, innermost
    /// last: a binding name maps to the access path it stands for.
    renames: Vec<HashMap<String, String>>,
    /// Source ranges whose output the ship artifact blanks: type-only imports.
    ship_blanks: Vec<(u32, u32)>,
    /// The source range of each top-level import and the names it binds.
    /// One that only tests read leaves the ship artifact with them.
    top_imports: Vec<(u32, u32, Vec<String>)>,
    /// `local _m1 = require(...) ` for each import below the top level
    /// of a spec, to write on the first line. See `require_text`.
    head_requires: Vec<String>,
    /// Declared struct names, for pattern tests and `is`.
    structs: HashSet<String>,
    /// The type parameters of a struct or an enum as the source writes
    /// them, `<T>`, for the ones that take any.
    /// The struct, enum, and namespace names a use precedes. The first
    /// line declares them, and their declaration assigns in place.
    hoisted: Vec<String>,
    /// The function names a use precedes. The first line declares
    /// them, and `function f()` fills the slot the way Luau reads it.
    hoisted_fns: Vec<String>,
    /// The namespace members a sibling body names above their
    /// declaration. The namespace header declares each one.
    ns_hoisted: Vec<NsHoist>,
    struct_generics: HashMap<String, String>,
    /// The structs whose `impl` writes a constructor, `new` or `New`, by
    /// its name: they construct through it, and the fields form stays
    /// inside their own impl.
    structs_with_new: HashMap<String, String>,
    /// The struct whose `impl` renders now, whose own raw constructor is
    /// the constructor's business.
    impl_target: Option<String>,
    /// The name of the method that renders now inside that `impl`. A
    /// `new Self()` in the constructor itself would call the
    /// constructor again, so the emit builds the value instead.
    impl_method: Option<String>,
    /// The `self` type of each top-level table a colon method is
    /// written on, for the check artifact.
    table_selfs: HashMap<String, crate::tables::SelfType>,
    /// The type of the `self` parameter the next function header has to
    /// write out, for a method the source spells with a colon.
    self_inject: Option<String>,
    /// Type names the file declares at the top level, so an ambient type
    /// of the same name yields to them anywhere in the file.
    declared_types: HashSet<String>,
    /// Declared trait names with their default-method names.
    traits: HashMap<String, Vec<String>>,
    /// The `self` of each `self:m()` in the trait default under render
    /// that calls through `__impl`, by token index; see `trait_decl`.
    self_dispatch: HashSet<u32>,
    /// Declared trait names with the methods an impl must write: name,
    /// parameter count with `self` included, and the return type the
    /// signature declares.
    trait_required: HashMap<String, Vec<(String, usize, Option<String>)>>,
    /// Declared struct fields by struct name: field name and whether it
    /// carries a default.
    struct_fields: HashMap<String, Vec<(String, bool)>>,
    /// Structs and enums declared with type parameters: their alias
    /// needs arguments, so the check artifact leaves `self` untyped
    /// there and casts no value to the bare name.
    generic_types: HashSet<String>,
    /// The instance methods of each struct an `impl` block writes, and
    /// the generic list of that block. The check artifact spells them
    /// into a generic struct's alias.
    struct_methods: HashMap<String, Vec<MethodSig>>,
    /// The generic list each `impl` block declares, by target.
    impl_generics: HashMap<String, String>,
    /// The trait each parameter of a bounded function asks of its
    /// argument, by the function's name and the parameter's place.
    /// `largest<T: Ord>(xs: { T })` asks `Ord` of its first argument.
    fn_bounds: HashMap<String, Vec<Option<String>>>,
    /// The same, for a method an `impl` block writes, by the target and
    /// the method's name. `impl Holder as function needsBoth<T: A & B>`
    /// asks `A & B` of the parameter typed `T`.
    method_bounds: HashMap<(String, String), Vec<Option<String>>>,
    /// The traits every `impl Trait for X` of this file meets, by target.
    impl_traits: HashMap<String, Vec<String>>,
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
    /// A `~T` in the file: the first line declares `__neg`.
    uses_neg: bool,
    /// Expansions so far, for the unique names of a body's locals.
    macro_serial: u32,
    /// True while a macro call that stands alone as a statement
    /// expands. Its body's statements stay statements, so a `return`
    /// in the body returns from the function around the call.
    macro_stmt: bool,
    /// Whether a live statement follows the statement the block walk
    /// renders. A macro body that ends in `return` has to be last.
    macro_followed: bool,
    /// Structs whose impl writes `to_string`: they print through it, so
    /// the default printer stays out.
    structs_with_to_string: HashSet<String>,
    /// The structs of this file that derive `Serialize`. A field of one
    /// of them serializes through its own `to_table`.
    serializable: HashSet<String>,
    /// The structs of this file that derive `Deserialize`. A field of
    /// one of them reads back through its own `from_table`.
    deserializable: HashSet<String>,
    /// The structs of this file that derive `Clone`: a field of one
    /// clones through its own `clone`.
    cloneable: HashSet<String>,
    /// The structs and enums of this file that derive `Eq` or
    /// `PartialEq`, so `==` compares their content.
    equatable: HashSet<String>,
    /// The structs of this file that derive `Default`: a field of one
    /// starts as its own `default()`.
    defaultable: HashSet<String>,
    /// The std names this file imports under their own names. Each
    /// renders as `__alloy.Name`, the way an ambient one does.
    std_imports: HashSet<String>,
    /// The locals a star import of the std binds, `import * as s`, each
    /// with the module it names; the facade is `""`.
    std_namespaces: HashMap<String, String>,
    /// Each alias an import gives a std name, `Signal as Sig`, with the
    /// name.
    std_aliases: HashMap<String, String>,
    /// The std names already reported as missing their import. The first
    /// use carries the report, and its fix writes the one line.
    std_reported: HashSet<String>,
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
    /// The namespaces this file declares, by key: the name for a
    /// top-level one, the path with `_` for the dot for a nested one.
    namespaces: HashMap<String, NamespaceInfo>,
    /// The name a namespace member renders under, by the token index of
    /// its own name. `struct Vec2` in `namespace Math` is `Math_Vec2`.
    member_names: HashMap<u32, String>,
    /// The namespaces under render, outermost first. Each one carries
    /// the scope depth its body opened at, so a local of the body
    /// shadows a member and a local outside it does not.
    ns_stack: Vec<NsFrame>,
    /// The names a top-level `export { ... }` list carries, so a
    /// namespace it names exports its types too.
    export_listed: HashSet<String>,
    /// The names an `export { }` list with no `from` sends out under
    /// their own name. An imported namespace among them sends its type
    /// aliases out with the word `export`.
    export_listed_bare: HashSet<String>,
    /// The names an `export { ... } from` list sends out, `default`
    /// aside. The type of a re-exported default takes none of them.
    reexported_types: HashSet<String>,
    /// The types a top-level `export { ... }` list names under their
    /// own names. Luau has no way to re-export an alias, so the
    /// declaration takes the `export` word instead.
    export_listed_types: HashSet<String>,
    /// The types the top level declares without `export`, and whether
    /// each is a value too: a struct is, an interface is not.
    file_types: HashMap<String, bool>,
    /// The types the file imports, under the name they bind here, and
    /// whether each is a value too. An `export { ... }` list may send
    /// one of them on, and a barrel module is nothing else.
    imported_types: HashMap<String, bool>,
    /// The next function header takes `local`: a namespace member never
    /// leaks into the file, and a plain `function f()` would be a Luau
    /// global. The attributed path reads it, since the modifier goes
    /// after the attribute lines.
    ns_force_local: bool,
    /// The namespace under render exports, so its type members carry
    /// `export` and another module can name them.
    ns_export: bool,
    /// A `@test` reaches the namespace under render, so each of its
    /// public functions is a test. The flag travels into a public
    /// nested namespace and stops at a private member.
    ns_test: bool,
    /// Every bare type name a type span holds, as the parser read them.
    /// A namespace's type renders under its own name, so the copy has
    /// to know where each name sits.
    type_name_spans: Vec<TokSpan>,
    /// The spans that name a member, not a value: types, struct fields,
    /// enum variants. The hoist scan reads tokens, and a name in there
    /// is no use of the declaration below it.
    non_value_spans: Vec<TokSpan>,
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
/// One `try do` block under render.
#[derive(Clone)]
pub(crate) struct TryTarget {
    /// The name the emit gives the block's `fail`.
    fail: String,
    /// Whether a `fail` call may pass its payload with the payload's own
    /// type. Luau reads the type of an unannotated parameter off the
    /// first call, so two error sources it cannot prove equal would pin
    /// `E` to the first and report the second. Those pass `any`, which
    /// leaves `E` open the way it was before.
    exact: bool,
}

pub(crate) enum Child<'a> {
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

/// Whether `test` holds for `e` or for a part of it that runs with it.
/// A function literal runs later, so the walk skips it.
pub(crate) fn any_part(e: &Expr, test: &impl Fn(&Expr) -> bool) -> bool {
    if matches!(e, Expr::Function { .. }) {
        return false;
    }

    test(e)
        || expr_children(e)
            .iter()
            .any(|c| matches!(c, Child::Expr(x) if any_part(x, test)))
}

/// Whether `e` itself calls code in place: a call, a constructor, an
/// await, a block, a child lookup, or a macro, whose body may hold any
/// of them. A `try` calls its operand in a hoist, which runs first.
pub(crate) fn calls_code(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Call { .. }
            | Expr::New { .. }
            | Expr::Await { .. }
            | Expr::TryBlock { .. }
            | Expr::AsyncBlock { .. }
            | Expr::Child { .. }
            | Expr::Macro { .. }
    )
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

pub(crate) fn expr_children(e: &Expr) -> Vec<Child<'_>> {
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

        Expr::Block { block, .. } => vec![Child::Block(block)],

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

pub(crate) fn stmt_children(s: &Stmt) -> Vec<Child<'_>> {
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

        Stmt::Destroy { expr, delay, .. } => {
            let mut v = vec![Child::Expr(expr)];

            if let Some(d) = delay {
                v.push(Child::Expr(d));
            }

            v
        }

        Stmt::After(a) => {
            let mut v = vec![Child::Expr(&a.delay)];

            if let Some(f) = &a.filter {
                v.push(Child::Expr(f));
            }

            v.push(Child::Block(&a.block));

            v
        }

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

        // A namespace renders its members itself, one at a time, so the
        // stitch never reaches them.
        Stmt::Namespace(_) => Vec::new(),

        Stmt::Attributed { stmt, .. } => stmt_children(stmt),
    }
}

/// The token span of everything under these statements that names a
/// member instead of a value: a type, a struct field, an enum variant.
/// `{ start: () -> () }`, `coins: number`, and `Begin` write a name the
/// shape owns, and no runtime read of that name is in there.
fn stmts_non_value_spans(stmts: &[Stmt], out: &mut Vec<TokSpan>) {
    for s in stmts {
        let mut bare = s.under_default();

        while let Stmt::Attributed { stmt, .. } = bare {
            bare = stmt.under_default();
        }

        match bare {
            Stmt::TypeAlias(t) => out.push(t.span),

            Stmt::Struct(d) => out.extend(d.fields.iter().flat_map(|f| [f.name, f.ty])),

            Stmt::Enum(d) => out.extend(
                d.variants
                    .iter()
                    .flat_map(|v| std::iter::once(v.name).chain(v.payload.iter().copied())),
            ),

            Stmt::Local(l) => out.extend(l.names.iter().filter_map(|b| b.ty)),

            // A namespace renders its members itself, so the child walk
            // skips them; the members still hold types.
            Stmt::Namespace(ns) => {
                for m in &ns.members {
                    stmts_non_value_spans(std::slice::from_ref(&m.stmt), out);
                }
            }

            _ => {}
        }

        for c in stmt_children(s) {
            child_non_value_spans(c, out);
        }
    }
}

fn child_non_value_spans(c: Child<'_>, out: &mut Vec<TokSpan>) {
    match c {
        Child::Expr(e) => {
            for c in expr_children(e) {
                child_non_value_spans(c, out);
            }
        }

        Child::Block(b) => stmts_non_value_spans(&b.stmts, out),

        Child::Function(f) => {
            out.extend(f.params.iter().filter_map(|p| p.ty));
            out.extend(f.ret_type);
            stmts_non_value_spans(&f.block.stmts, out);
        }
    }
}

/// The token span of every function body under these statements. A use
/// inside one runs after the declaration it reads.
fn stmts_function_spans(stmts: &[Stmt], out: &mut Vec<TokSpan>) {
    for s in stmts {
        // A namespace renders its members itself, so the child walk
        // skips them; the bodies are still bodies.
        if let Stmt::Namespace(ns) = s {
            for m in &ns.members {
                stmts_function_spans(std::slice::from_ref(&m.stmt), out);
            }

            continue;
        }

        for c in stmt_children(s) {
            child_function_spans(c, out);
        }
    }
}

fn child_function_spans(c: Child<'_>, out: &mut Vec<TokSpan>) {
    match c {
        Child::Expr(e) => {
            for c in expr_children(e) {
                child_function_spans(c, out);
            }
        }

        Child::Block(b) => stmts_function_spans(&b.stmts, out),

        Child::Function(f) => {
            out.push(f.span);
            stmts_function_spans(&f.block.stmts, out);
        }
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
        Stmt::Namespace(_) => return true,

        // `@cfg` writes the guard, and any other attribute here reports.
        Stmt::Attributed { .. } => return true,

        // The trailing expression of a `try do` or `async do` block:
        // the emit writes the `return` the source leaves out.
        Stmt::Return(r) if r.value_only => return true,

        Stmt::Assign(a) if a.op.end - a.op.start == 3 => return true,

        Stmt::Local(l) if local_needs_rewrite(l) => return true,

        Stmt::Delete { .. } | Stmt::Destroy { .. } | Stmt::After(_) => return true,

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
        // `class` has no lowering yet. The render reports it and blanks
        // the block, and it only runs when the walk reaches it.
        | Stmt::Class(_)
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

        // A list sends a value out, or a type or a macro, which bind
        // none. A list of those alone still makes an export table.
        Stmt::ExportList(_) => true,

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

        // A written `<<T>>` lowers on every call: the ship drops it,
        // and the check artifact spells it the Luau way.
        Expr::Index { .. } | Expr::Call { .. }
            if chain_has_alloy(e) || expressions::chain_has_type_args(e) =>
        {
            return true;
        }

        _ => {}
    }

    expr_children(e).iter().any(|c| match c {
        Child::Expr(e) => expr_needs_desugar(e),

        Child::Block(b) => b.stmts.iter().any(stmt_needs_desugar),

        Child::Function(f) => f.block.stmts.iter().any(stmt_needs_desugar),
    })
}

/// Backticked names joined with commas and a final `and`.
/// `a` or `an` for the word a report puts after it. The test reads both
/// cases, or a type named `E` takes `a`.
pub fn article(word: &str) -> &'static str {
    match word.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        true => "an",

        false => "a",
    }
}

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

    /// The struct, enum, namespace, and function names a function body
    /// reads above their declaration. Such a function would call a nil
    /// global `Point`; the first line opens the table instead, and the
    /// declaration fills it. A use that runs at the top level before the
    /// declaration reads an empty table, so it reports.
    fn scan_hoisted(&mut self, block: &Block) {
        let mut bodies = Vec::new();
        stmts_function_spans(&block.stmts, &mut bodies);

        // A macro body expands at each `$name` call, so the call is the
        // use and the body itself is quiet. An import entry binds a
        // name and reads none; the duplicate check owns that pair.
        let macros: Vec<(&str, TokSpan)> = block
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Macro(m) => Some((self.text_of(m.name), m.span)),

                _ => None,
            })
            .collect();
        let quiet: Vec<TokSpan> = block
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Macro(m) => Some(m.span),

                Stmt::Import(i) => Some(i.span),

                _ => None,
            })
            .collect();

        // The field types of each struct with a derive that reads them:
        // `from_table` sets the metatable of a payload enum, and a nested
        // struct clones and serializes through its own functions. The
        // derived functions run after the file loads, as a body does.
        let derived: Vec<(u32, &str)> = block
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Struct(st)
                    if st
                        .attributes
                        .iter()
                        .filter(|a| a.name.is_some_and(|n| self.text_of(n) == "derive"))
                        .flat_map(|a| a.args.iter())
                        .any(|arg| {
                            matches!(
                                self.derive_name(arg).as_str(),
                                "Clone" | "Default" | "Serialize" | "Deserialize"
                            )
                        }) =>
                {
                    Some(st)
                }

                _ => None,
            })
            .flat_map(|st| st.fields.iter())
            .flat_map(|f| f.ty.start..f.ty.end)
            .map(|k| (k, self.toks[k as usize].text(self.src)))
            .collect();

        // The names declared so far.
        let mut declared: Vec<&str> = Vec::new();

        for s in &block.stmts {
            let (name, decl, kind) = match s {
                Stmt::Struct(d) => (d.name, d.span, "struct"),

                Stmt::Enum(d) => (d.name, d.span, "enum"),

                Stmt::Namespace(d) => (d.name, d.span, "namespace"),

                Stmt::Function(f) if f.path.len() == 1 => (f.path[0], f.span, "function"),

                Stmt::LocalFunction(f) => (f.name, f.span, "function"),

                _ => continue,
            };
            let name = self.text_of(name);
            let is_fn = kind == "function";
            // A use between two declarations of one name reads the
            // earlier one, which is valid Luau; only the first
            // declaration can sit below its use.
            let earlier = declared.contains(&name);
            let mut deferred = false;

            declared.push(name);

            if earlier {
                continue;
            }

            let reads = |k: usize| self.reads_name(k, name, is_fn);
            // A macro that calls another macro expands what that one
            // reads too, down to the depth the expansion stops at.
            let expands = |k: usize| {
                if k == 0 || self.toks[k - 1].text(self.src) != "$" {
                    return false;
                }

                let mut todo = vec![(self.toks[k].text(self.src), 0usize)];
                let mut seen: Vec<&str> = Vec::new();

                while let Some((called, depth)) = todo.pop() {
                    if depth >= 16 || seen.contains(&called) {
                        continue;
                    }

                    seen.push(called);

                    let Some((_, span)) = macros.iter().find(|(m, _)| *m == called) else {
                        continue;
                    };

                    for j in span.start as usize..span.end as usize {
                        if reads(j) {
                            return true;
                        }

                        if j > 0 && self.toks[j - 1].text(self.src) == "$" {
                            todo.push((self.toks[j].text(self.src), depth + 1));
                        }
                    }
                }

                false
            };

            for k in 0..decl.start as usize {
                if quiet
                    .iter()
                    .any(|span| (span.start as usize..span.end as usize).contains(&k))
                    || !(reads(k) || expands(k))
                {
                    continue;
                }

                if bodies
                    .iter()
                    .any(|b| (b.start as usize..b.end as usize).contains(&k))
                {
                    deferred = true;
                    continue;
                }

                let message =
                    format!("`{name}` is declared below this use; move the {kind} above it");
                self.diagnose(TokSpan::new(k, k + 1), &message);
                break;
            }

            deferred |= !is_fn
                && derived
                    .iter()
                    .any(|(k, word)| *k < decl.start && *word == name);

            if deferred && is_fn {
                self.hoisted_fns.push(name.to_string());
            } else if deferred {
                self.hoisted.push(name.to_string());
            }
        }
    }

    /// Whether the token at `k` reads `name` as a value.
    ///
    /// A field, `x.Point`, and a type, `p: Point`, read no value; the
    /// alias is in scope over the whole block. A declaration head of
    /// the name is the duplicate check's. A method call, `o:tag()`, a
    /// key, `{ tag = 1 }`, and an assignment target, `tag = 1`, name a
    /// slot of something else, whatever the declaration below is. A
    /// member of a type or of a declaration, `{ tag: () -> () }`, names
    /// no value at all.
    pub(crate) fn reads_name(&self, k: usize, name: &str, is_fn: bool) -> bool {
        let t = self.toks[k];
        let before = if k > 0 {
            self.toks[k - 1].text(self.src)
        } else {
            ""
        };
        let after = self.toks.get(k + 1).map_or("", |t| t.text(self.src));

        t.kind == TokKind::Ident
            && t.text(self.src) == name
            && after != "="
            && !(is_fn && matches!(before, "function" | "local"))
            && !self
                .non_value_spans
                .iter()
                .any(|s| (s.start..s.end).contains(&(k as u32)))
            && !matches!(
                before,
                "." | ":"
                    | "struct"
                    | "enum"
                    | "namespace"
                    | "impl"
                    | "for"
                    | "trait"
                    | "class"
                    | "interface"
                    | "type"
                    | "attribute"
                    | "remote"
                    | "macro"
                    | "$"
            )
            && !self.type_name_spans.iter().any(|s| s.start as usize == k)
    }

    /// Whether a line above the declaration declared the function: the
    /// file's first line, or the header of the namespace it belongs to.
    pub(crate) fn is_hoisted_fn(&self, name: TokSpan) -> bool {
        if self.ns_hoisted.iter().any(|h| h.name_tok == name.start) {
            return true;
        }

        self.ns_stack.is_empty() && self.hoisted_fns.iter().any(|h| h == self.text_of(name))
    }

    /// Whether the walk stands at the file's top level, outside any
    /// namespace: the statements the module owns.
    pub(crate) fn at_top_level(&self) -> bool {
        self.scopes.len() == self.top_scope + 1 && self.ns_stack.is_empty()
    }

    /// `local Name = {} ` for a declaration, or nothing for one the
    /// first line opened.
    fn decl_head(&self, name: &str) -> String {
        if self.hoisted.iter().any(|h| h == name) {
            String::new()
        } else {
            format!("local {name} = {{}} ")
        }
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

        // A namespace's type renders under a name of its own, and a
        // type slot outside the namespace writes the path. Both read
        // the same name here.
        if let Some((s, e, text)) = self.namespace_type_at(start, end) {
            self.r.copy(start, s);
            self.generate(s, &text);
            self.copy(e, end);

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

            TypeEdit::Negation { tildes, operand } => {
                self.byte_start(*tildes) >= start && self.byte_end(*operand) <= end
            }

            TypeEdit::TypeOf(span) => {
                self.byte_start(*span) >= start && self.byte_end(*span) <= end
            }
        });

        let edit = match edit {
            Some(TypeEdit::AmbientName(span)) => {
                let (ns, ne) = (self.byte_start(span), self.byte_end(span));
                let name = self.text_of(span).to_string();
                self.r.copy(start, ns);

                if self.is_local(&name) || self.declared_types.contains(&name) {
                    self.r.copy(ns, ne);

                    return self.copy(ne, end);
                }

                if !self.options.definitions {
                    self.check_std_name(span, &name);
                }

                if self.options.definitions {
                    // Luau loads a definitions file on its own, with no
                    // require, and one unknown type drops the whole file.
                    let fix = match name.as_str() {
                        "Array" => "write `T[]` or `{ T }`",

                        _ => "write the type out",
                    };
                    let message = format!(
                        "`{name}` is a type of the Alloy std, and a definitions file cannot reach the std; {fix}"
                    );
                    self.diagnose(span, &message);
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

            // `~T` is `__neg<T>`: Luau's parser has no negation, and its
            // type function library builds one. The function sits in the
            // file itself: one exported from the runtime reaches another
            // module unchecked, and the negation would accept anything.
            Some(TypeEdit::Negation { tildes, operand }) => {
                let (ts, os, oe) = (
                    self.byte_start(tildes),
                    self.byte_start(operand),
                    self.byte_end(operand),
                );
                self.r.copy(start, ts);
                self.uses_neg = true;
                self.generate(ts, "__neg<");
                self.copy(os, oe);
                self.generate(oe, ">");
                self.copy(oe, end);

                return;
            }

            // Only the calls change, so the rest of the `typeof` keeps
            // its bytes and its lines.
            Some(TypeEdit::TypeOf(span)) => {
                let base = self.byte_start(span);
                let text = self.text_of(span).to_string();
                let mut at = start;

                for (s, e, require) in self.typeof_imports(&text) {
                    self.r.copy(at, base + s);
                    self.generate(base + s, &require);
                    at = base + e;
                }

                self.copy(at, end);

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

        // A host gives a plain table, with no std metatable behind it,
        // so a definitions file writes the Luau array type.
        if self.options.definitions {
            let read = modifier.is_some_and(|m| self.text_of(m) == "read");
            self.generate(edit_start, if read { "{ read [number]: " } else { "{ " });
            self.copy(op_s, op_e);
            self.generate(op_e, " }");
            self.copy(br_e, end);

            return;
        }

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

    /// The first byte a type edit starts at inside a range, if any.
    fn earliest_edit_start(&self, start: u32, end: u32) -> Option<u32> {
        self.type_edits
            .iter()
            .filter_map(|e| {
                let (s, x) = match e {
                    TypeEdit::ArraySuffix {
                        modifier,
                        operand,
                        brackets,
                    } => (
                        match modifier {
                            Some(m) => self.byte_start(*m),

                            None => self.byte_start(*operand),
                        },
                        self.byte_end(*brackets),
                    ),

                    TypeEdit::AmbientName(span) => (self.byte_start(*span), self.byte_end(*span)),

                    TypeEdit::Mapped { table, .. } => {
                        (self.byte_start(*table), self.byte_end(*table))
                    }

                    TypeEdit::Negation { tildes, operand } => {
                        (self.byte_start(*tildes), self.byte_end(*operand))
                    }

                    TypeEdit::TypeOf(span) => (self.byte_start(*span), self.byte_end(*span)),
                };

                (s >= start && x <= end).then_some(s)
            })
            .min()
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
        // A namespace member is in scope under its rendered name, so a
        // local of the member's own name still shadows it.
        let name = self.decl_name(span);

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
            None => {
                self.record_binding_type(b.name, b.ty);
                self.declare_name(b.name);
            }

            Some(d) => self.declare_destructure(d),
        }
    }

    /// The annotation a binding carries, under its name. The map is
    /// flat, so a name the file binds twice keeps its type only when
    /// both bindings write the same one. A binding with no annotation
    /// blanks it: the name then stands for a type no reader knows.
    fn record_binding_type(&mut self, name: TokSpan, ty: Option<TokSpan>) {
        let text = ty.map(|ty| self.text_of(ty).trim().trim_start_matches(':').trim());
        self.record_type_text(name, text);
    }

    /// `record_binding_type` with the type as text, for a type that the
    /// value gives and no annotation writes.
    pub(crate) fn record_type_text(&mut self, name: TokSpan, text: Option<&str>) {
        let key = self.text_of(name).to_string();
        let Some(text) = text else {
            self.binding_types.insert(key, String::new());

            return;
        };

        match self.binding_types.get(&key) {
            Some(old) if old != text => {
                self.binding_types.insert(key, String::new());
            }

            Some(_) => {}

            None => {
                self.binding_types.insert(key, text.to_string());
            }
        }
    }

    fn declare_params(&mut self, body: &FunctionBody) {
        for p in &body.params {
            match &p.destructure {
                None => {
                    self.record_binding_type(p.name, p.ty);
                    self.declare_name(p.name);
                }

                Some(d) => self.declare_destructure(d),
            }
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    /// How many scopes stand open. A namespace frame keeps the depth of
    /// its body, so `is_local_since` can tell a local of the body from
    /// one of the file around it.
    pub(crate) fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Whether a name is a local declared at or past `depth`.
    pub(crate) fn is_local_since(&self, depth: usize, name: &str) -> bool {
        self.scopes.iter().skip(depth).any(|s| s.contains(name))
    }

    /// Whether a declaration opens with the removed `global` keyword.
    /// The emit writes `export` in its place, so the artifact the
    /// checker reads is valid Luau even on a file that reports.
    pub(crate) fn wrote_global(&self, span: TokSpan) -> bool {
        self.toks.get(span.start as usize).map(|t| t.text(self.src)) == Some("global")
    }

    /// Reports each `global` this file wrote. The word left the
    /// language; the report names the `export` declaration and the
    /// `import` that replace it. See the globals-removal RFC.
    fn report_globals(&mut self, src: &str, toks: &[Tok], chunk: &Chunk) {
        // The module every reader writes in its `import`. The file that
        // declares the name is the module that holds it.
        let stem = self
            .options
            .file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .split('.')
            .next()
            .unwrap_or_default();
        let spec = match stem.is_empty() {
            true => "./that-module".to_string(),

            false => format!("./{stem}"),
        };

        for span in &chunk.global_keywords {
            let Some(word) = toks.get(span.start as usize) else {
                continue;
            };
            // The words between `global` and the name it declares:
            // `local`, `local function`, `remote function`, `open class`.
            let mut at = span.start as usize + 1;
            let mut words: Vec<&str> = Vec::new();

            while let Some(t) = toks.get(at) {
                let text = t.text(src);

                if !DECL_WORDS.contains(&text) {
                    break;
                }

                if text != "export" {
                    words.push(text);
                }

                at += 1;
            }

            let kind = words.join(" ");
            let name = toks.get(at).map(|t| t.text(src)).unwrap_or_default();
            // An attribute is written `@tag` wherever it is applied,
            // and an import list writes it the same way.
            let imported = match kind.as_str() {
                "attribute" => format!("@{name}"),

                _ => name.to_string(),
            };
            let message = match kind.as_str() {
                // An `export impl` already reaches every file that
                // imports the module, and it binds no name of its own.
                "impl" => {
                    "`global` is removed; `export impl` reaches every file that imports this module"
                        .to_string()
                }

                // A macro expands where it is written, and no import
                // carries one, so it stays in the file that declares it.
                "macro" => "`global` is removed; a macro is in scope in the file that declares it"
                    .to_string(),

                _ => format!(
                    "`global` is removed; declare `{name}` with `export {kind}` and write `import {{ {imported} }} from \"{spec}\"` where it is read"
                ),
            };
            self.diagnostics.push(Diagnostic {
                start: word.start,
                end: word.end,
                message,
            });
        }
    }

    // --- blocks and statements --------------------------------------------

    /// The next temp number the source does not name.
    fn bump_temp(&mut self) -> u32 {
        loop {
            self.temp_next += 1;

            if !self.taken_temps.contains(&self.temp_next) {
                return self.temp_next;
            }
        }
    }

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
        self.render_side(|d| d.expr(e))
    }

    /// Runs `render` against its own renderer and returns that renderer,
    /// chunks and all, so a caller can append it with provenance.
    fn render_side(&mut self, render: impl FnOnce(&mut Self)) -> Renderer<'s> {
        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        render(self);
        std::mem::swap(&mut self.r, &mut side);

        side
    }

    /// Writes hoists as statements in front of what follows. With
    /// `fresh`, every temp is a new local: inside a closure, the block's
    /// temps are upvalues, and two coroutines must not share one.
    fn write_hoists(&mut self, hoists: Vec<Hoist<'s>>, fresh: bool) {
        for h in hoists {
            match h {
                Hoist::Temp {
                    index,
                    value,
                    anchor,
                } => {
                    let keyword = if fresh {
                        "local "
                    } else if self.temp_declared(index) {
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

                Hoist::Stmt { text, anchor, .. } => {
                    let line = format!("{text} ");
                    self.generate(anchor, &line);
                }

                Hoist::Fresh {
                    name,
                    value,
                    anchor,
                } => match value {
                    HoistValue::Text(text) => {
                        let line = format!("local {name} = {text} ");
                        self.generate(anchor, &line);
                    }

                    HoistValue::Rendered(rendered) => {
                        self.generate(anchor, &format!("local {name} = "));
                        self.r.append(rendered);
                        self.generate(anchor, " ");
                    }
                },
            }

            self.r.end_stmt();
        }
    }

    /// Renders `e`, as an operand that runs on some paths only when
    /// `lazy` is set.
    pub(crate) fn expr_lazy(&mut self, lazy: bool, e: &Expr) {
        let saved = std::mem::replace(&mut self.lazy, lazy);
        self.expr(e);
        self.lazy = saved;
    }

    /// `render_to_string` for an operand that runs on some paths only.
    pub(crate) fn render_lazy(&mut self, e: &Expr) -> String {
        self.render_side(|d| d.expr_lazy(true, e)).finish().0
    }

    /*
    Renders `e` where a hoist in front of the statement would change what
    it means: `e` runs on some paths only, or an earlier part of the
    statement must run first. The hoists `e` asks for stay inside it, as
    `(function() local _1 = v return e end)()` on the line `e` starts.
    With no hoist, `e` renders as it is.

    A `try` that returns cannot return from inside a closure. On a path
    that may skip it, that is an error; ahead of an earlier call, its
    hoists still go in front of the statement.
    */
    pub(crate) fn expr_in_place(&mut self, e: &Expr, render: impl FnOnce(&mut Self)) {
        let saved_hoists = std::mem::take(&mut self.hoists);
        let lazy = std::mem::replace(&mut self.lazy, false);
        let effects = std::mem::replace(&mut self.effects, false);
        let reads = std::mem::replace(&mut self.reads, false);
        let in_place = std::mem::replace(&mut self.in_place, true);
        let side = self.render_side(render);
        let hoists = std::mem::replace(&mut self.hoists, saved_hoists);
        self.lazy = lazy;
        self.effects |= effects;
        self.reads |= reads;
        self.in_place = in_place;

        if hoists.is_empty() {
            self.r.append(side);

            return;
        }

        if hoists
            .iter()
            .any(|h| matches!(h, Hoist::Stmt { exits: true, .. }))
        {
            if lazy {
                self.diagnose(
                    e.span(),
                    "`try` cannot return from an operand that runs on some paths only, such as the right side of `and` or a loop condition; bind it to a local first",
                );
            }

            self.hoists.extend(hoists);
            self.r.append(side);

            return;
        }

        let anchor = self.byte_start(e.span());
        // A closure reads no `...` of the function around it.
        let (open, close) = if any_part(e, &|x| matches!(x, Expr::Vararg(_))) {
            ("(function(...) ", " end)(...)")
        } else {
            ("(function() ", " end)()")
        };
        self.generate(anchor, open);
        self.write_hoists(hoists, true);
        self.generate(anchor, "return ");
        self.r.append(side);
        self.generate(self.byte_end(e.span()), close);
    }

    /// Hoists rendered text into a temp and returns the temp's name.
    fn hoist_text(&mut self, value: String, anchor: u32) -> String {
        self.bump_temp();
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
        if self.require_at_head() {
            return self.require_text(path);
        }

        let name = self.next_import_temp();
        self.hoists.push(Hoist::Fresh {
            name: name.clone(),
            value: HoistValue::Text(format!("require({path})")),
            anchor,
        });

        name
    }

    fn next_import_temp(&mut self) -> String {
        self.import_next += 1;

        while self.taken_temps.contains(&self.import_next) {
            self.import_next += 1;
        }

        format!("_m{}", self.import_next)
    }

    /// Whether an import here requires its module on the first line. A
    /// spec runs under lest, and its native backend resolves a `require`
    /// only while the spec loads. An import in a test runs later.
    fn require_at_head(&self) -> bool {
        self.options.tests && !self.at_top_level()
    }

    /// `require(path)`, or the temp that the first line of a spec binds
    /// to it. See `require_at_head`.
    pub(crate) fn require_text(&mut self, path: &str) -> String {
        if !self.require_at_head() {
            return format!("require({path})");
        }

        let name = self.next_import_temp();
        self.head_requires
            .push(format!("local {name} = require({path}) "));

        name
    }

    /// Hoists a whole statement in front of the current one.
    fn hoist_stmt(&mut self, text: String, anchor: u32, exits: bool) {
        self.hoists.push(Hoist::Stmt {
            text,
            anchor,
            exits,
        });
    }

    /// Hoists an expression into a temp and returns the temp's name. The
    /// expression renders with its provenance, so one that spans lines,
    /// a function literal as an argument, keeps every line in place.
    fn hoist(&mut self, e: &Expr) -> String {
        let anchor = self.byte_start(e.span());
        let rendered = self.render_hoisted(|d| d.render_to_side(e));

        self.hoist_rendered(rendered, anchor)
    }

    /// Renders a value that a hoist takes. The hoists run in order in
    /// front of the statement, so what the value calls or reads is not
    /// an earlier part of the statement for a later hoist.
    fn render_hoisted<T>(&mut self, render: impl FnOnce(&mut Self) -> T) -> T {
        let flags = (self.effects, self.reads);
        let value = render(self);
        (self.effects, self.reads) = flags;

        value
    }

    /// Hoists a rendered value, chunks and all, into a temp and returns
    /// the temp's name.
    fn hoist_rendered(&mut self, rendered: Renderer<'s>, anchor: u32) -> String {
        self.bump_temp();
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

    #[test]
    fn a_try_inside_a_try_block_calls_the_block_fail() {
        let src = "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\nlocal r = try do\n    local v = try parse(\"1\")\n    return v + 1\nend\nprint(r)\n";
        let out = crate::compile_with(src, &EmitOptions::default()).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("try_block(function(__fail)"),
            "{}",
            out.ship
        );
        assert!(out.ship.contains("__fail(_1._1, _1.trace)"), "{}", out.ship);
        // The Err leaves through `fail`, never through a `return`, which
        // would decide the closure's value type instead.
        assert!(!out.ship.contains("then return _1 end"), "{}", out.ship);
    }

    #[test]
    fn a_try_block_inside_a_plain_function_needs_no_result_return_type() {
        let src = "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\nlocal function count(): number\n    local r = try do\n        local v = try parse(\"1\")\n        return v\n    end\n    return r:unwrap_or(0)\nend\nprint(count)\n";
        let out = crate::compile_with(src, &EmitOptions::default()).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    }

    #[test]
    fn two_error_types_a_file_can_name_as_different_pass_any() {
        let same = "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\nlocal r = try do\n    local a = try parse(\"1\")\n    local b = try parse(\"2\")\n    return a + b\nend\nprint(r)\n";
        let out = crate::compile_with(same, &EmitOptions::default()).unwrap();

        assert!(!out.ship.contains(":: any"), "{}", out.ship);

        let mixed = "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\nlocal function code(s: string): Result<string, number>\n    return Ok(\"x\")\nend\nlocal r = try do\n    local a = try parse(\"1\")\n    local b = try code(\"2\")\n    return a + #b\nend\nprint(r)\n";
        let options = EmitOptions {
            check: true,
            ..EmitOptions::default()
        };
        let out = crate::compile_with(mixed, &options).unwrap();

        // Luau reads an unannotated parameter's type off the first call,
        // so a second error type would report; `any` leaves `E` open.
        assert_eq!(
            out.check.matches("(_1._1 :: any)").count(),
            2,
            "{}",
            out.check
        );
    }

    #[test]
    fn a_block_inside_a_block_names_its_own_fail() {
        let src = "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\nlocal r = try do\n    local inner = try do\n        local v = try parse(\"1\")\n        return v\n    end\n    return inner:unwrap_or(0)\nend\nprint(r)\n";
        let out = crate::compile_with(src, &EmitOptions::default()).unwrap();

        assert!(out.ship.contains("function(__fail)"), "{}", out.ship);
        assert!(out.ship.contains("function(__fail2)"), "{}", out.ship);
        assert!(
            out.ship.contains("__fail2(_1._1, _1.trace)"),
            "{}",
            out.ship
        );
    }

    /*
    A value block ends in an expression, whose value is the block's. The
    reader took it only when no statement stood in front of it, so
    `try do if c then break end x end` reported `this expression is not a
    statement` at the `x`. It now reads the expression after any
    statement, a block that ends in `end` included.
    */
    /// `await f()` stands alone as a statement, so the statement parser
    /// takes it; at the end of a value block it is still the value.
    #[test]
    fn a_trailing_await_is_the_value_of_the_block() {
        let src = "async function slow(): number
    return 9
end

async function main()
    local c = try do
        await (slow())
    end
    print(c)
end
main()
";
        let out = crate::compile_with(src, &EmitOptions::default())
            .unwrap()
            .ship;

        assert!(out.contains("return __alloy.await((slow()))"), "{out}");
    }

    #[test]
    fn a_value_block_takes_its_trailing_expression_after_any_statement() {
        let ship = |src: &str| -> String {
            crate::compile_with(src, &EmitOptions::default())
                .unwrap()
                .ship
        };

        // A `break` in a nested `if`, then the value.
        let src = "local function scan(xs: number[])
    for _, x in xs do
        local r = try do
            if x > 2 then
                break
            end
            x
        end
        print(r)
    end
end
scan([1])
";
        let out = ship(src);

        assert!(out.contains("return x"), "{out}");

        // A `continue` and a `return` read the same way, and `async do`
        // takes a trailing expression too.
        let src = "local function scan(xs: number[])
    for _, x in xs do
        local r = async do
            if x > 2 then
                continue
            end
            x + 1
        end
        print(r)
    end
end
scan([1])
";
        let out = ship(src);

        assert!(out.contains("return x + 1"), "{out}");

        // A `do ... end` block in front of the value, and a local.
        let src = "local function f(n: number)
    local r = try do
        local a = n
        do
            print(a)
        end
        a + 1
    end
    print(r)
end
f(1)
";
        let out = ship(src);

        assert!(out.contains("return a + 1"), "{out}");

        // A nested block takes no trailing expression of its own: the
        // `n` inside the `if` is still no statement.
        let src = "local function f(n: number)
    local r = try do
        if n > 1 then
            n
        end
        n
    end
    print(r)
end
f(1)
";

        let messages: Vec<String> = crate::compile(src)
            .unwrap()
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect();

        assert_eq!(messages, vec!["this expression is not a statement"]);
    }
}
