/*!
The Luau syntax tree. Nodes hold token index ranges, not owned strings. So a
print of a node is a replay of its tokens and the source between them. The
tree keeps types as spans on purpose. Larvae parses types but does not
interpret them until a rule needs to.
*/

/// A half-open range of token indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokSpan {
    pub start: u32,
    pub end: u32,
}

impl TokSpan {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[derive(Debug)]
pub struct Chunk {
    pub block: Block,
    /// Alloy type syntax found inside type spans, for emit to rewrite.
    pub type_edits: Vec<TypeEdit>,
}

/// A piece of Alloy syntax inside a type span. Types stay spans, so the
/// parser records what emit must rewrite instead of building a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeEdit {
    /// `T[]`: `operand` is the type before the brackets, `brackets` the
    /// `[` `]` pair. With a modifier, `read T[]` or `write T[]`.
    ArraySuffix {
        modifier: Option<TokSpan>,
        operand: TokSpan,
        brackets: TokSpan,
    },
    /// A std type name used bare in a type: `Result<T, E>`, `Future<T>`,
    /// `Array<T>`, `HashMap<K, V>`, `Set<T>`. Emit qualifies it.
    AmbientName(TokSpan),
    /// `{ [K in keyof T]: V }`: the whole table type span, the key name,
    /// the source type name, the value shape, and the value's modifier.
    Mapped {
        table: TokSpan,
        key: TokSpan,
        source: TokSpan,
        modifier: Option<TokSpan>,
        /// `T[K]` alone, or `T[K]?`.
        optional: bool,
    },
}

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum Stmt {
    /// A stray `;`.
    Empty(TokSpan),
    Local(Local),
    Assign(Assign),
    /// A call used as a statement.
    Call(Expr, TokSpan),
    Do(DoBlock),
    While(While),
    Repeat(Repeat),
    If(If),
    NumericFor(NumericFor),
    GenericFor(GenericFor),
    Function(Function),
    LocalFunction(LocalFunction),
    Return(Return),
    Break(TokSpan),
    Continue(TokSpan),
    TypeAlias(TypeAlias),
    /// A `declare` statement of a definitions file; the span is the whole statement
    Declare(Declare),
    Class(Class),
    /// `import ... from "path"`.
    Import(Import),
    /// `export { a, b as c }` and `export { a } from "path"`.
    ExportList(ExportList),
    /// `export default expr`.
    ExportDefault {
        value: Expr,
        span: TokSpan,
    },
    /// `enum Name as ... end`.
    Enum(EnumDecl),
    /// `impl Name ... end` and `impl Trait for Name ... end`.
    Impl(ImplDecl),
    /// `match e with case ... default ... end` as a statement.
    Match(MatchStmt),
    /// `local Pat(x) = e` and its let-else form.
    PatternLocal(PatternLocal),
    /// `struct Name as fields end`.
    Struct(StructDecl),
    /// `trait Name ... end`.
    Trait(TraitDecl),
    /// `interface Name extends A as fields end`.
    Interface(InterfaceDecl),
    /// `remote Name(params) from client`.
    Remote(RemoteDecl),
    /// `attribute name(params) on targets`.
    Attribute(AttributeDecl),
    /// `macro name(params) ... end`.
    Macro(MacroDecl),
    /// `delete expr`, which is `expr:Destroy()`.
    Delete {
        expr: Expr,
        span: TokSpan,
    },
    /// Tokens the lenient parser could not read. The span tiles the block
    /// like any statement, so the printer still reproduces the source.
    Error(TokSpan),
}

impl Stmt {
    pub fn span(&self) -> TokSpan {
        match self {
            Stmt::Empty(s)
            | Stmt::Break(s)
            | Stmt::Continue(s)
            | Stmt::Error(s)
            | Stmt::Call(_, s)
            | Stmt::Delete { span: s, .. }
            | Stmt::ExportDefault { span: s, .. } => *s,

            Stmt::Import(n) => n.span,

            Stmt::ExportList(n) => n.span,

            Stmt::Enum(n) => n.span,

            Stmt::Impl(n) => n.span,

            Stmt::Match(n) => n.span,

            Stmt::PatternLocal(n) => n.span,

            Stmt::Struct(n) => n.span,

            Stmt::Trait(n) => n.span,

            Stmt::Interface(n) => n.span,

            Stmt::Remote(n) => n.span,

            Stmt::Attribute(n) => n.span,

            Stmt::Macro(n) => n.span,

            Stmt::Local(n) => n.span,

            Stmt::Assign(n) => n.span,

            Stmt::Do(n) => n.span,

            Stmt::Class(n) => n.span,

            Stmt::While(n) => n.span,

            Stmt::Repeat(n) => n.span,

            Stmt::If(n) => n.span,

            Stmt::NumericFor(n) => n.span,

            Stmt::GenericFor(n) => n.span,

            Stmt::Function(n) => n.span,

            Stmt::LocalFunction(n) => n.span,

            Stmt::Return(n) => n.span,

            Stmt::TypeAlias(n) => n.span,

            Stmt::Declare(n) => n.span,
        }
    }
}

/// One `@name` or `@name(args)` attribute, parsed. Upstream's `@[...]`
/// group keeps only its span.
#[derive(Debug)]
pub struct Attr {
    pub name: Option<TokSpan>,
    pub args: Vec<Expr>,
    pub span: TokSpan,
}

/// `struct Name<T> as read x: number = 1 end`.
#[derive(Debug)]
pub struct StructDecl {
    pub attributes: Vec<Attr>,
    pub exported: bool,
    pub name: TokSpan,
    pub generics: Option<TokSpan>,
    pub fields: Vec<Field>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Field {
    pub attributes: Vec<Attr>,
    /// `private` or `public`; a field is public without one.
    pub visibility: Option<TokSpan>,
    /// `read` or `write`.
    pub modifier: Option<TokSpan>,
    pub name: TokSpan,
    pub ty: TokSpan,
    pub default: Option<Expr>,
    pub span: TokSpan,
}

/// `trait Name ... end`: signatures, some with default bodies.
#[derive(Debug)]
pub struct TraitDecl {
    pub attributes: Vec<Attr>,
    pub exported: bool,
    pub name: TokSpan,
    pub methods: Vec<TraitMethod>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct TraitMethod {
    pub name: TokSpan,
    /// The parameter list and return type, as a span from `(`.
    pub signature: TokSpan,
    pub params: Vec<Param>,
    /// A default implementation.
    pub body: Option<FunctionBody>,
    pub span: TokSpan,
}

/// `interface Name extends A, B as fields end`.
#[derive(Debug)]
pub struct InterfaceDecl {
    pub exported: bool,
    pub name: TokSpan,
    pub generics: Option<TokSpan>,
    pub extends: Vec<TokSpan>,
    pub fields: Vec<Field>,
    pub span: TokSpan,
}

/// `remote Name(params) from client`, `remote function Name(params) -> T
/// from server`.
#[derive(Debug)]
pub struct RemoteDecl {
    pub attributes: Vec<Attr>,
    pub exported: bool,
    pub is_function: bool,
    pub name: TokSpan,
    pub params: Vec<Param>,
    pub ret_type: Option<TokSpan>,
    pub from_client: bool,
    pub from_server: bool,
    pub span: TokSpan,
}

/// `attribute name(params) on targets`.
#[derive(Debug)]
pub struct AttributeDecl {
    pub exported: bool,
    pub name: TokSpan,
    pub params: Vec<Param>,
    pub targets: Vec<TokSpan>,
    pub span: TokSpan,
}

/// `macro name(params) ... end`: a template with expression parameters.
#[derive(Debug)]
pub struct MacroDecl {
    pub exported: bool,
    pub name: TokSpan,
    pub params: Vec<Param>,
    pub body: Block,
    /// A trailing expression after the statements.
    pub tail: Option<Expr>,
    pub span: TokSpan,
}

/// `import * as M from "p"`, `import M from "p"`, `import { a, type T,
/// b as c } from "p"`, `import type { T } from "p"`.
#[derive(Debug)]
pub struct Import {
    pub kind: ImportKind,
    /// The string token of the path.
    pub path: TokSpan,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum ImportKind {
    /// `* as M` and `M`: the whole module.
    Namespace(TokSpan),
    /// `{ a, b as c, type T }`.
    Named(Vec<ImportSpec>),
    /// `type { T, U }`: erased in the ship artifact.
    TypeOnly(Vec<ImportSpec>),
}

#[derive(Debug, Clone, Copy)]
pub struct ImportSpec {
    pub name: TokSpan,
    pub alias: Option<TokSpan>,
    pub is_type: bool,
}

#[derive(Debug)]
pub struct ExportList {
    pub specs: Vec<ImportSpec>,
    /// `export { a } from "p"`.
    pub from: Option<TokSpan>,
    /// `export type { T } from "p"`.
    pub type_only: bool,
    pub span: TokSpan,
}

/// `enum Name as Variant(T, U) Other = 1 Unit end`.
#[derive(Debug)]
pub struct EnumDecl {
    pub attributes: Vec<Attr>,
    pub exported: bool,
    pub name: TokSpan,
    pub variants: Vec<Variant>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Variant {
    pub name: TokSpan,
    /// The payload types, as spans.
    pub payload: Vec<TokSpan>,
    /// `Foo = 1`, upstream's explicit value.
    pub value: Option<Expr>,
    pub span: TokSpan,
}

/// `impl Name ... end`, `impl Trait for Name ... end`.
#[derive(Debug)]
pub struct ImplDecl {
    pub exported: bool,
    pub trait_name: Option<TokSpan>,
    /// The dotted target name.
    pub target: TokSpan,
    pub methods: Vec<Function>,
    pub span: TokSpan,
}

/// `match a, b with case p, q and guard then ... default ... end`.
#[derive(Debug)]
pub struct MatchStmt {
    pub scrutinees: Vec<Expr>,
    pub arms: Vec<MatchArm>,
    pub default: Option<Block>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct MatchArm {
    pub patterns: Vec<Pattern>,
    pub guard: Option<Expr>,
    pub block: Block,
    pub span: TokSpan,
}

/// The expression form: every arm is one expression.
#[derive(Debug)]
pub struct MatchExpr {
    pub scrutinees: Vec<Expr>,
    pub arms: Vec<MatchExprArm>,
    pub default: Option<Box<Expr>>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct MatchExprArm {
    pub patterns: Vec<Pattern>,
    pub guard: Option<Expr>,
    pub value: Expr,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum Pattern {
    /// `_`.
    Wildcard(TokSpan),
    /// A bare name: binds, unless it names a unit variant in scope.
    Bind(TokSpan),
    /// A number, string, boolean, nil, or negative number.
    Literal(Box<Expr>),
    /// `Enum.KeyCode.W`, `Color.Red`: compared with `==`.
    Path(TokSpan),
    /// `Variant(p, q)`.
    Variant {
        name: TokSpan,
        args: Vec<Pattern>,
        span: TokSpan,
    },
    /// `Struct { a, b = p }` or `{ a, b = p }`.
    Struct {
        name: Option<TokSpan>,
        fields: Vec<FieldPattern>,
        span: TokSpan,
    },
    /// `[ a, b, ...rest ]`.
    Array {
        items: Vec<Pattern>,
        rest: Option<TokSpan>,
        span: TokSpan,
    },
    /// `p or q`.
    Or(Box<Pattern>, Box<Pattern>, TokSpan),
}

impl Pattern {
    pub fn span(&self) -> TokSpan {
        match self {
            Pattern::Wildcard(s) | Pattern::Bind(s) | Pattern::Path(s) => *s,

            Pattern::Literal(e) => e.span(),

            Pattern::Variant { span, .. }
            | Pattern::Struct { span, .. }
            | Pattern::Array { span, .. }
            | Pattern::Or(_, _, span) => *span,
        }
    }
}

#[derive(Debug)]
pub struct FieldPattern {
    pub field: TokSpan,
    /// `field = pattern`; a bare field binds itself.
    pub pattern: Option<Pattern>,
}

/// `local Ok(v) = expr` and `local Ok(v) = expr else ... end`.
#[derive(Debug)]
pub struct PatternLocal {
    pub keyword: TokSpan,
    pub pattern: Pattern,
    pub value: Expr,
    pub else_block: Option<Block>,
    pub span: TokSpan,
}

/// The condition of an `if`, `elseif`, `while`, or `if` expression.
#[derive(Debug)]
pub enum Cond {
    Expr(Expr),
    /// `local x = e`, `not local x = e`, `local p = f(); local q = g(p)`,
    /// `local x = e where c`, and pattern forms like `local Ok(v) = e`.
    Local {
        negated: bool,
        bindings: Vec<CondBinding>,
        filter: Option<Expr>,
        span: TokSpan,
    },
}

impl Cond {
    pub fn span(&self) -> TokSpan {
        match self {
            Cond::Expr(e) => e.span(),

            Cond::Local { span, .. } => *span,
        }
    }
}

#[derive(Debug)]
pub struct CondBinding {
    pub is_const: bool,
    pub pattern: Pattern,
    pub ty: Option<TokSpan>,
    pub value: Expr,
}

/// `local a, b: T = x, y` or its `const` form.
#[derive(Debug)]
pub struct Local {
    /// Parsed attributes, `@name(args)`, on the declaration.
    pub attrs: Vec<Attr>,
    /// The `local` or `const` keyword.
    pub keyword: TokSpan,
    pub is_const: bool,
    /// `export local x = 1`; the module exposes the binding by value.
    pub exported: bool,
    pub names: Vec<Binding>,
    pub values: Vec<Expr>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Binding {
    /// The name token, or the whole `{ ... }` / `[ ... ]` of a destructure.
    pub name: TokSpan,
    pub ty: Option<TokSpan>,
    /// `local { a, b = c } = t` or `local [ x, ...rest ] = t`.
    pub destructure: Option<Destructure>,
}

#[derive(Debug)]
pub enum Destructure {
    /// `{ a, b = c }`: field `a` binds `a`; field `b` binds `c`.
    Table(Vec<FieldBinding>),
    /// `[ a, b, ...rest ]`.
    Array {
        items: Vec<TokSpan>,
        rest: Option<TokSpan>,
    },
}

#[derive(Debug)]
pub struct FieldBinding {
    pub field: TokSpan,
    /// The local name when it differs from the field.
    pub rename: Option<TokSpan>,
}

/// `a, b = x, y` and the compound forms, for example `a += 1`.
#[derive(Debug)]
pub struct Assign {
    pub targets: Vec<Expr>,
    /// The `=` or compound operator.
    pub op: TokSpan,
    pub values: Vec<Expr>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct DoBlock {
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct While {
    pub cond: Cond,
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Repeat {
    pub block: Block,
    pub cond: Expr,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct If {
    /// The `if` and every `elseif`. Each entry is a condition and a body.
    pub branches: Vec<(Cond, Block)>,
    pub else_block: Option<Block>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct NumericFor {
    pub var: Binding,
    pub start: Expr,
    pub limit: Expr,
    pub step: Option<Expr>,
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct GenericFor {
    pub vars: Vec<Binding>,
    pub exprs: Vec<Expr>,
    /// `for ... in t where cond do`: skip an iteration when false.
    pub filter: Option<Expr>,
    pub block: Block,
    pub span: TokSpan,
}

/// `function a.b:c() end`.
#[derive(Debug)]
pub struct Function {
    pub attributes: Vec<TokSpan>,
    /// The same attributes, parsed.
    pub attrs: Vec<Attr>,
    /// `export function f()`; the module exposes the function by value.
    pub exported: bool,
    /// `private function` or `public function` in an `impl`.
    pub visibility: Option<TokSpan>,
    /// Every name token in the dotted path, with the `:` method name included.
    pub path: Vec<TokSpan>,
    pub is_method: bool,
    pub body: FunctionBody,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct LocalFunction {
    pub attributes: Vec<TokSpan>,
    /// The same attributes, parsed.
    pub attrs: Vec<Attr>,
    /// `export local function f()`; accepted beside `export local`.
    pub exported: bool,
    /// `const function f()` instead of `local function f()`.
    pub is_const: bool,
    pub name: TokSpan,
    pub body: FunctionBody,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct FunctionBody {
    /// `async function`: the `async` token.
    pub is_async: Option<TokSpan>,
    pub generics: Option<TokSpan>,
    /// A generic carries a bound, `<T: Shape>`; emit rewrites it.
    pub has_bounds: bool,
    pub params: Vec<Param>,
    pub ret_type: Option<TokSpan>,
    /// The return type came after `->` instead of `:`.
    pub ret_arrow: Option<TokSpan>,
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Param {
    /// The name token, or the `...` token for a vararg param.
    pub name: TokSpan,
    pub is_vararg: bool,
    pub ty: Option<TokSpan>,
    /// `x: number = 1`.
    pub default: Option<Expr>,
    /// `{ a, b }` or `[ a, b ]` in parameter position.
    pub destructure: Option<Destructure>,
}

#[derive(Debug)]
pub struct Return {
    pub values: Vec<Expr>,
    pub span: TokSpan,
}

/*
A class declaration, `[export] class Name ... end`.

The RFC keeps the body to two member forms: a field, `[public] name [: T]`,
and a method, an ordinary function with one name. Inheritance is deferred
there, so no `extends` exists here either.
*/
#[derive(Debug)]
pub struct Class {
    pub exported: bool,
    /// `open class`; the inheritance RFC lets an open class be extended.
    pub open: bool,
    pub name: TokSpan,
    /// The base class name in `class Name extends Base`.
    pub extends: Option<TokSpan>,
    pub members: Vec<ClassMember>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum ClassMember {
    Field {
        public: bool,
        name: TokSpan,
        ty: Option<TokSpan>,
        span: TokSpan,
    },
    Method(Function),
}

/*
One declaration of a `.d.luau` definitions file.

`declare function`, `declare name: T`, and `declare class ... end` all
land here. The parser validates the inner structure; the tree keeps the
span whole, because a declaration is meta code that larvae reads and
never rewrites.
*/
#[derive(Debug)]
pub struct Declare {
    pub span: TokSpan,
}

/// `type X<T> = ...`, `export type ...`, `type function f() ... end`.
#[derive(Debug)]
pub struct TypeAlias {
    pub exported: bool,
    pub name: TokSpan,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum Expr {
    Nil(TokSpan),
    True(TokSpan),
    False(TokSpan),
    Vararg(TokSpan),
    Number(TokSpan),
    String(TokSpan),
    /// A backtick string with no hole.
    InterpString(TokSpan),
    /// A backtick string with holes; `parts` are the hole expressions in
    /// order, and the text segments sit in the tokens between them.
    Interp {
        parts: Vec<Expr>,
        span: TokSpan,
    },
    Name(TokSpan),
    Function {
        attributes: Vec<TokSpan>,
        body: Box<FunctionBody>,
        span: TokSpan,
    },

    Table {
        fields: Vec<TableField>,
        span: TokSpan,
    },

    Binary {
        op: TokSpan,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: TokSpan,
    },

    Unary {
        op: TokSpan,
        operand: Box<Expr>,
        span: TokSpan,
    },

    Paren {
        inner: Box<Expr>,
        span: TokSpan,
    },

    /// `obj.field` or `obj[key]`; `obj?.field` and `obj?[key]` when optional.
    Index {
        object: Box<Expr>,
        key: IndexKey,
        /// The `?` before the link: nil when the object is nil.
        optional: bool,
        span: TokSpan,
    },

    /// `f(args)`, `obj:method(args)`, `f<<T>>(args)`; `f?(args)` and
    /// `obj?:method(args)` when optional.
    Call {
        func: Box<Expr>,
        method: Option<TokSpan>,
        /// The `?` before the link: nil when the callee is nil.
        optional: bool,
        /*
        An explicit type instantiation: the `<<T>>` in `f<<T>>()`.

        The tree holds it instead of a discard, because each rebuild of
        source from the tree must put it back. A discard turned
        `charm.atom<<number>>()` into `charm.atom()`. That result still
        parses, but it no longer means the same thing. So the loss stayed
        invisible until some code printed the tree.
        */
        type_args: Option<TokSpan>,
        args: CallArgs,
        span: TokSpan,
    },

    /// `if c then a else b` as an expression.
    IfElse {
        branches: Vec<(Cond, Expr)>,
        else_value: Box<Expr>,
        span: TokSpan,
    },

    /// `match e with case p then v default d end` as an expression.
    Match(Box<MatchExpr>),

    /// `expr :: T`.
    TypeAssert {
        expr: Box<Expr>,
        ty: TokSpan,
        span: TokSpan,
    },

    /// `obj->Name` is `FindFirstChild`; `obj=>Name` is `WaitForChild`.
    Child {
        object: Box<Expr>,
        name: ChildName,
        /// `=>` waits; `->` finds and is nil-guarded.
        wait: bool,
        span: TokSpan,
    },

    /// `expr!`: the value must not be nil.
    NonNil {
        operand: Box<Expr>,
        span: TokSpan,
    },

    /// `c ? a : b`.
    Ternary {
        cond: Box<Expr>,
        then_value: Box<Expr>,
        else_value: Box<Expr>,
        span: TokSpan,
    },

    /// `x is T`, `x is not T`. The type is a dotted name.
    Is {
        expr: Box<Expr>,
        negated: bool,
        name: TokSpan,
        span: TokSpan,
    },

    /// `expr satisfies T`.
    Satisfies {
        expr: Box<Expr>,
        ty: TokSpan,
        span: TokSpan,
    },

    /// `[ a, b, c ]`.
    Array {
        items: Vec<Expr>,
        span: TokSpan,
    },

    /// `obj:name` with no call: a bound method reference.
    MethodRef {
        object: Box<Expr>,
        name: TokSpan,
        span: TokSpan,
    },

    /// `new Name<<T>>(args) { init }`.
    New {
        /// The dotted name after `new`.
        name: Box<Expr>,
        type_args: Option<TokSpan>,
        args: Option<CallArgs>,
        /// The initializer table.
        init: Option<Box<Expr>>,
        span: TokSpan,
    },

    /// `await expr`.
    Await {
        operand: Box<Expr>,
        span: TokSpan,
    },

    /// `try expr`.
    Try {
        operand: Box<Expr>,
        span: TokSpan,
    },

    /// `async do ... end`.
    AsyncBlock {
        block: Block,
        span: TokSpan,
    },

    /// `try do ... end`.
    TryBlock {
        block: Block,
        span: TokSpan,
    },

    /// `$name(args)` or `$M.name(args)`: an intrinsic or macro call.
    Macro {
        /// The dotted name after `$`.
        name: TokSpan,
        args: Vec<Expr>,
        span: TokSpan,
    },
}

/// What follows `->` or `=>`.
#[derive(Debug)]
pub enum ChildName {
    /// `->Map`.
    Name(TokSpan),
    /// `->"Spawn Points"` or an interpolated string.
    Str(TokSpan),
    /// `->[expr]`.
    Computed(Box<Expr>),
}

impl Expr {
    pub fn span(&self) -> TokSpan {
        match self {
            Expr::Nil(s)
            | Expr::True(s)
            | Expr::False(s)
            | Expr::Vararg(s)
            | Expr::Number(s)
            | Expr::String(s)
            | Expr::InterpString(s)
            | Expr::Name(s) => *s,
            Expr::Function { span, .. }
            | Expr::Table { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Interp { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Index { span, .. }
            | Expr::Call { span, .. }
            | Expr::IfElse { span, .. }
            | Expr::TypeAssert { span, .. }
            | Expr::Child { span, .. }
            | Expr::NonNil { span, .. }
            | Expr::Ternary { span, .. }
            | Expr::Is { span, .. }
            | Expr::Satisfies { span, .. }
            | Expr::Array { span, .. }
            | Expr::MethodRef { span, .. }
            | Expr::New { span, .. }
            | Expr::Await { span, .. }
            | Expr::Try { span, .. }
            | Expr::AsyncBlock { span, .. }
            | Expr::TryBlock { span, .. }
            | Expr::Macro { span, .. } => *span,

            Expr::Match(m) => m.span,
        }
    }
}

#[derive(Debug)]
pub enum IndexKey {
    /// `.name`.
    Field(TokSpan),
    /// `[expr]`.
    Computed(Box<Expr>),
}

#[derive(Debug)]
pub enum CallArgs {
    Paren(Vec<Expr>),
    /// `f "str"`.
    Str(TokSpan),
    /// `f {table}`.
    Table(Box<Expr>),
}

#[derive(Debug)]
pub enum TableField {
    /// `value`.
    Positional(Expr),
    /// `name = value`.
    Named { name: TokSpan, value: Expr },
    /// `[key] = value`.
    Computed { key: Expr, value: Expr },
    /// `...base`.
    Spread(Expr),
}
