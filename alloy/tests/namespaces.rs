//! `namespace Name as ... end`: one name over a group of declarations.
//!
//! The emit gives each member a name of its own and puts it on a table,
//! so `Math.clamp(x)` stays `Math.clamp(x)` and `Math.Vec2` in a type
//! slot reads `Math_Vec2`. These tests compile single files and small
//! projects and read the emitted text and the diagnostics.

use std::fs;
use std::path::{Path, PathBuf};

use alloy::config::Config;

/// Compiles one file and gives the ship artifact, the check artifact,
/// and the diagnostic messages back.
fn compile(src: &str) -> (String, String, Vec<String>) {
    let options = alloy::EmitOptions {
        file_name: "t.aly".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with(src, &options).unwrap();
    let messages = out.diagnostics.iter().map(|d| d.message.clone()).collect();

    (out.ship, out.check, messages)
}

fn ship(src: &str) -> String {
    compile(src).0
}

/// The diagnostics of one file.
fn messages(src: &str) -> Vec<String> {
    compile(src).2
}

#[track_caller]
fn clean(src: &str) -> String {
    let (ship, _, messages) = compile(src);
    assert!(messages.is_empty(), "diagnostics {messages:?}");
    assert_eq!(
        ship.lines().count(),
        src.lines().count(),
        "line count changed"
    );

    ship
}

fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-namespaces-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"out\"\n").unwrap();

    dir
}

fn build(dir: &Path) -> alloy::build::Report {
    let config = Config::load(&dir.join("alloy.toml")).unwrap();

    alloy::build::run_project(dir, &config).unwrap()
}

fn output(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join("out").join(rel)).unwrap()
}

// --- 1. what a namespace emits ----------------------------------------------

/// The header opens a table and each member lands on it.
#[test]
fn the_header_opens_a_table_and_members_land_on_it() {
    let out = clean("namespace Math as\n    const PI = 3.14\nend\n\nprint(Math.PI)\n");
    assert!(out.contains("local Math = {}"), "{out}");
    assert!(out.contains("Math.PI = Math_PI"), "{out}");
    assert!(out.contains("print(Math.PI)"), "{out}");
}

/// A sibling needs no prefix inside the namespace.
#[test]
fn a_sibling_reads_without_the_prefix() {
    let out = clean(
        "namespace Math as\n    function one(): number\n        return 1\n    end\n    function two(): number\n        return one() + one()\n    end\nend\n\nprint(Math.two())\n",
    );
    assert!(out.contains("return Math_one() + Math_one()"), "{out}");
}

/// A member never leaks into the file, so a plain `function` takes the
/// `local` a Luau global would not have.
#[test]
fn a_member_function_is_local() {
    let out = clean("namespace M as\n    function f()\n    end\nend\n\nM.f()\n");
    assert!(out.contains("local function M_f()"), "{out}");
}

/// Luau has no `Math.Vec2` type path, so the struct declares
/// `Math_Vec2` and every type slot reads that name.
#[test]
fn a_type_of_a_namespace_renders_under_one_name() {
    let out = clean(
        "namespace Math as\n    struct Vec2 as\n        x: number\n    end\nend\n\nlocal v: Math.Vec2 = { x = 1 }\n\nprint(v)\n",
    );
    assert!(out.contains("type Math_Vec2 ="), "{out}");
    assert!(out.contains("local v: Math_Vec2"), "{out}");
}

/// A plain `type` member takes the same name.
#[test]
fn a_type_alias_member_renders_under_one_name() {
    let out = clean("namespace M as\n    type Id = number\nend\n\nlocal a: M.Id = 1\n\nprint(a)\n");
    assert!(out.contains("type M_Id = number"), "{out}");
    assert!(out.contains("local a: M_Id = 1"), "{out}");
}

/// A message names the path the author wrote, never the emitted name.
#[test]
fn a_runtime_message_names_the_path() {
    let out =
        clean("namespace M as\n    struct P as\n        x: number\n    end\nend\n\nprint(M.P)\n");
    assert!(out.contains("show_struct(\"M.P\""), "{out}");
    assert!(!out.contains("show_struct(\"M_P\""), "{out}");
}

/// The inner namespace is a field of the outer table, and its members
/// take both names.
#[test]
fn a_nested_namespace_is_a_field_of_the_outer_one() {
    let out = clean(
        "namespace Outer as\n    namespace Inner as\n        const B = 2\n        struct Point as\n            x: number\n        end\n    end\nend\n\nlocal p: Outer.Inner.Point = { x = Outer.Inner.B }\n\nprint(p)\n",
    );
    assert!(out.contains("Outer.Inner = {}"), "{out}");
    assert!(out.contains("Outer.Inner.B = Outer_Inner_B"), "{out}");
    assert!(out.contains("local p: Outer_Inner_Point"), "{out}");
}

/// An `impl` inside a namespace targets the namespace's own struct.
#[test]
fn an_impl_inside_a_namespace_targets_the_member() {
    let out = clean(
        "namespace M as\n    struct P as\n        x: number\n    end\n    impl P as\n        function get(self): number\n            return self.x\n        end\n    end\nend\n\nlocal p = new M.P { x = 1 }\n\nprint(p:get())\n",
    );
    assert!(out.contains("function M_P.get(self)"), "{out}");
}

/// An `impl` on a type from outside the namespace keeps its own name.
#[test]
fn an_impl_on_a_foreign_target_keeps_its_name() {
    let out = clean(
        "struct P as\n    x: number\nend\n\nnamespace M as\n    impl P as\n        function get(self): number\n            return self.x\n        end\n    end\nend\n\nlocal p = new P { x = 1 }\n\nprint(p:get())\n",
    );
    assert!(out.contains("function P.get(self)"), "{out}");
}

/// A `match` over a namespaced enum reads the variant with no prefix
/// inside the namespace, and by its path outside.
#[test]
fn a_match_over_a_namespaced_enum_covers_it() {
    let inside = clean(
        "namespace M as\n    enum K as\n        A\n        B\n    end\n    function name(k: K): string\n        match k with\n            case A then return \"a\"\n            case B then return \"b\"\n        end\n\n        return \"\"\n    end\nend\n\nprint(M.name(M.K.A))\n",
    );
    assert!(inside.contains("if _m1 == \"A\" then"), "{inside}");

    let outside = clean(
        "namespace M as\n    enum K as\n        A\n        B\n    end\nend\n\nmatch M.K.A with\n    case M.K.A then print(\"a\")\n    case M.K.B then print(\"b\")\nend\n",
    );
    assert!(outside.contains("_m1 == M.K.A"), "{outside}");
}

/// A local of a member's name shadows the member inside the body.
#[test]
fn a_local_shadows_a_member_of_the_same_name() {
    let out = clean(
        "local x = 1\n\nnamespace M as\n    const x = 2\n    function f(): number\n        local x = 3\n\n        return x\n    end\nend\n\nprint(x, M.x, M.f())\n",
    );
    assert!(out.contains("local x = 3"), "{out}");
    assert!(out.contains("return x\n"), "{out}");
    assert!(out.contains("print(x, M.x, M.f())"), "{out}");
}

/// A name from the scope around the namespace still reads.
#[test]
fn a_member_reaches_the_scope_around_the_namespace() {
    let out =
        clean("local MAX = 100\n\nnamespace M as\n    const cap = MAX\nend\n\nprint(M.cap)\n");
    assert!(out.contains("const M_cap = MAX"), "{out}");
}

// --- 2. visibility ----------------------------------------------------------

/// A private member stays a local, so nothing reaches it from outside.
#[test]
fn a_private_member_stays_off_the_table() {
    let out = clean(
        "namespace M as\n    private const secret = 1\n    function get(): number\n        return secret\n    end\nend\n\nprint(M.get())\n",
    );
    assert!(!out.contains("M.secret ="), "{out}");
    assert!(out.contains("return M_secret"), "{out}");
}

/// A use of a private member from outside reports, in the wording a
/// private struct field takes.
#[test]
fn a_private_member_named_from_outside_reports() {
    let hits = messages("namespace M as\n    private const secret = 1\nend\n\nprint(M.secret)\n");
    assert_eq!(hits, vec!["`secret` is private to `M`".to_string()]);
}

/// A private type reports from a type slot the same way.
#[test]
fn a_private_type_named_from_outside_reports() {
    let hits = messages(
        "namespace M as\n    private struct P as\n        x: number\n    end\nend\n\nlocal p: M.P = { x = 1 }\n\nprint(p)\n",
    );
    assert_eq!(hits, vec!["`P` is private to `M`".to_string()]);
}

/// `public` is the default and says so.
#[test]
fn public_is_the_default() {
    let plain = ship("namespace M as\n    const a = 1\nend\n");
    let marked = ship("namespace M as\n    public const a = 1\nend\n");
    assert_eq!(plain, marked);
}

// --- 3. what a namespace refuses --------------------------------------------

/// Two namespaces of one name in one file report.
#[test]
fn one_name_twice_in_a_file_reports() {
    let hits =
        messages("namespace M as\n    const a = 1\nend\n\nnamespace M as\n    const b = 2\nend\n");
    assert_eq!(
        hits,
        vec!["`M` is declared twice; a namespace has one name here".to_string()]
    );
}

/// `global` inside a namespace reports: the two reaches do not stack.
#[test]
fn a_global_inside_a_namespace_reports() {
    let hits = messages("namespace M as\n    global const a = 1\nend\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("`global` reaches every file"), "{hits:?}");
}

/// A namespace of one name in each of two namespaces is fine.
#[test]
fn one_name_under_two_namespaces_is_clean() {
    clean(
        "namespace A as\n    const x = 1\nend\n\nnamespace B as\n    const x = 2\nend\n\nprint(A.x, B.x)\n",
    );
}

// --- 4. across files --------------------------------------------------------

/// `export namespace` sends the whole group, and an import binds the
/// table and every type the namespace carries.
#[test]
fn an_exported_namespace_reaches_another_file() {
    let dir = temp_project("export");
    fs::write(
        dir.join("src/geom.aly"),
        "export namespace Geom as\n    const ORIGIN = 0\n    struct Point as\n        x: number\n        y: number\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import { Geom } from \"./geom\"\n\nlocal p: Geom.Point = { x = Geom.ORIGIN, y = 1 }\n\nprint(p)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let geom = output(&dir, "geom.luau");
    assert!(geom.contains("export type Geom_Point ="), "{geom}");
    assert!(geom.contains("return { Geom = Geom }"), "{geom}");

    let main = output(&dir, "main.luau");
    assert!(main.contains("type Geom_Point = _m1.Geom_Point"), "{main}");
    assert!(main.contains("local p: Geom_Point"), "{main}");
    assert!(main.contains("Geom.ORIGIN"), "{main}");
}

/// `export { Geom }` below the declaration exports the group too.
#[test]
fn an_export_list_sends_a_namespace() {
    let dir = temp_project("export-list");
    fs::write(
        dir.join("src/geom.aly"),
        "namespace Geom as\n    struct Point as\n        x: number\n    end\nend\n\nexport { Geom }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import { Geom } from \"./geom\"\n\nlocal p: Geom.Point = { x = 1 }\n\nprint(p)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let main = output(&dir, "main.luau");
    assert!(main.contains("type Geom_Point = _m1.Geom_Point"), "{main}");
}

/// `global namespace` reaches every file without an import.
#[test]
fn a_global_namespace_reaches_every_file() {
    let dir = temp_project("global");
    fs::write(
        dir.join("src/shared.aly"),
        "global namespace Math as\n    const PI = 3.14\n    struct Vec2 as\n        x: number\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "local v: Math.Vec2 = { x = Math.PI }\n\nprint(v)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let main = output(&dir, "main.luau");
    assert!(main.contains("local Math = _g1.Math"), "{main}");
    assert!(main.contains("type Math_Vec2 = _g1.Math_Vec2"), "{main}");
    assert!(main.contains("local v: Math_Vec2"), "{main}");
}

/// A private member of an exported namespace does not travel.
#[test]
fn a_private_member_does_not_export() {
    let dir = temp_project("private-export");
    fs::write(
        dir.join("src/geom.aly"),
        "export namespace Geom as\n    private struct Hidden as\n        x: number\n    end\n    struct Point as\n        x: number\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import { Geom } from \"./geom\"\n\nlocal p: Geom.Point = { x = 1 }\n\nprint(p)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let main = output(&dir, "main.luau");
    assert!(!main.contains("Geom_Hidden"), "{main}");
}

// --- 5. attributes ----------------------------------------------------------

/// `@deprecated` sits on a namespace; its line stays and the header
/// takes the line under it.
#[test]
fn a_namespace_takes_deprecated() {
    let out = clean(
        "@deprecated(\"use Geometry\")\nnamespace Old as\n    const x = 1\nend\n\nprint(Old.x)\n",
    );
    assert!(
        out.lines().nth(1).unwrap().contains("local Old = {}"),
        "{out}"
    );
}

/// `@cfg(server)` on a namespace: each member reaches the table on
/// that side only.
#[test]
fn a_cfg_on_a_namespace_guards_the_table() {
    let out =
        clean("@cfg(server)\nnamespace Store as\n    const key = 1\nend\n\nprint(Store.key)\n");
    assert!(
        out.contains("if __alloy.cfg.server() then Store.key = Store_key end"),
        "{out}"
    );
}

/// `@cfg` on a member is the member's own guard.
#[test]
fn a_cfg_on_a_member_guards_the_member() {
    let out = clean(
        "namespace Debug as\n    @cfg(server)\n    function log(msg: string)\n        print(msg)\n    end\nend\n\nDebug.log(\"a\")\n",
    );
    assert!(
        out.contains("local function Debug_log(msg: string)"),
        "{out}"
    );
    assert!(out.contains("cfg.server()"), "{out}");
}

/// A user attribute takes `namespace` as a target.
#[test]
fn a_user_attribute_reaches_a_namespace() {
    clean(
        "attribute tag(name: string) on namespace\n\n@tag(\"core\")\nnamespace M as\n    const x = 1\nend\n\nprint(M.x)\n",
    );
}

/// An attribute that does not take `namespace` reports there.
#[test]
fn an_attribute_that_misses_the_target_reports() {
    let hits = messages("@derive(Clone)\nnamespace M as\n    const x = 1\nend\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("has no meaning on a namespace"),
        "{hits:?}"
    );
}

// --- 6. the members a namespace takes ---------------------------------------

/// A trait with a default body, inside a namespace.
#[test]
fn a_trait_member_keeps_its_default_body() {
    let out = clean(
        "namespace M as\n    trait Shape as\n        function area(self): number\n        function describe(self): string\n            return \"shape\"\n        end\n    end\nend\n\nprint(M.Shape)\n",
    );
    assert!(out.contains("type M_Shape ="), "{out}");
    assert!(
        out.contains("function M_Shape.describe(self): string"),
        "{out}"
    );
}

/// `export type { Geom.Point as Position }` sends one type of a
/// namespace out under a name of its own.
#[test]
fn an_export_list_sends_one_type_of_a_namespace() {
    let out = clean(
        "namespace Geom as\n    type Point = { x: number, y: number }\nend\n\nexport type { Geom.Point as Position }\n",
    );
    assert!(out.contains("export type Position = Geom_Point"), "{out}");
}

/// A remote's parameter reads a struct of a namespace.
#[test]
fn a_remote_takes_a_namespaced_struct() {
    let (_, check, hits) = compile(
        "namespace Types as\n    struct Damage as\n        amount: number\n    end\nend\n\nexport remote Hit(damage: Types.Damage) from client\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
    assert!(
        check.contains("fire: (damage: Types_Damage) -> ()"),
        "{check}"
    );
}

/// A macro and an attribute run at compile time, so neither takes a
/// name of its own and neither reaches the table.
#[test]
fn a_compile_time_member_keeps_its_own_name() {
    let out = clean(
        "namespace M as\n    macro twice(x)\n        x * 2\n    end\n    attribute tag(name: string) on field\nend\n\nprint($twice(2))\n",
    );
    assert!(out.contains("print(2 * 2)"), "{out}");
    assert!(!out.contains("M.twice"), "{out}");
}

/// A namespace inside a function has nowhere to put its members: the
/// emit gives each one a name of the file.
#[test]
fn a_namespace_inside_a_block_reports() {
    let hits = messages(
        "function outer()\n    namespace Inner as\n        const A = 1\n    end\n\n    return Inner.A\nend\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("goes at the top level"), "{hits:?}");

    // A member's own body is a block too.
    let inner = messages(
        "namespace M as\n    function f()\n        namespace Bad as\n        end\n    end\nend\n",
    );
    assert_eq!(inner.len(), 1, "{inner:?}");
}

/// `alloy doc namespace` gives a member `public` or `private`. An
/// `export` there exported nothing and wrote `local local function`.
#[test]
fn export_on_a_member_reports_and_the_emit_stays_luau() {
    let hits = messages(
        "namespace M as\n    export function get(): number\n        return 1\n    end\nend\n\nprint(M.get())\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("`export` on a namespace member"),
        "{hits:?}"
    );

    let out = ship(
        "namespace M as\n    export function get(): number\n        return 1\n    end\nend\n\nprint(M.get())\n",
    );
    assert!(out.contains("local function M_get()"), "{out}");
    assert!(!out.contains("local local"), "{out}");
    assert!(!out.contains("return {"), "{out}");
}

/// The export table named the source name, which nothing binds.
#[test]
fn an_exported_namespace_returns_the_group_alone() {
    let out = ship(
        "export namespace Math as\n    const pi = 3\n\n    function twice(x: number): number\n        return x * 2\n    end\nend\n",
    );
    assert!(out.contains("return { Math = Math }"), "{out}");
    assert!(!out.contains("pi = pi"), "{out}");
}

/// A nested member reaches through the outer table: the `export` was
/// what broke `Outer.Inner.x`.
#[test]
fn a_nested_member_reaches_through_the_outer_table() {
    let out = clean(
        "namespace Outer as\n    namespace Inner as\n        const x = 1\n    end\nend\n\nprint(Outer.Inner.x)\n",
    );
    assert!(out.contains("Outer.Inner.x = Outer_Inner_x"), "{out}");
    assert!(out.contains("print(Outer.Inner.x)"), "{out}");
}

/// A member shadowing a name of the file wins inside the namespace.
#[test]
fn a_member_shadows_an_outer_local_inside_the_body() {
    let out = clean(
        "local x = 1\n\nnamespace Foo as\n    const x = 2\n\n    function get(): number\n        return x\n    end\nend\n\nprint(x, Foo.get())\n",
    );
    assert!(out.contains("return Foo_x"), "{out}");
    assert!(out.contains("print(x, Foo.get())"), "{out}");
}

/// A local of the body still shadows the member.
#[test]
fn a_local_of_the_body_shadows_the_member() {
    let out = clean(
        "namespace Foo as\n    const x = 2\n\n    function get(): number\n        local x = 5\n\n        return x\n    end\nend\n\nprint(Foo.get())\n",
    );
    assert!(out.contains("local x = 5"), "{out}");
    assert!(!out.contains("return Foo_x"), "{out}");
}

/// A member reads a name of the file that no member declares.
#[test]
fn a_member_reads_the_parent_scope() {
    let out = clean(
        "local MAX = 100\n\nnamespace Foo as\n    function get(): number\n        return MAX\n    end\nend\n\nprint(Foo.get())\n",
    );
    assert!(out.contains("return MAX"), "{out}");
}

/// `@deprecated` has no Luau form on a namespace, so a lint reports the
/// use. Inside the body a member reads a sibling and nothing fires.
#[test]
fn a_deprecated_namespace_reports_at_a_use() {
    let src = "@deprecated(\"use Geometry\")\nnamespace Old as\n    const x = 1\n\n    function get(): number\n        return x\n    end\nend\n\nprint(Old.get())\n";
    let options = alloy::EmitOptions {
        file_name: "t.aly".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with(src, &options).unwrap();
    let hits: Vec<&alloy::lint::Lint> = out
        .lints
        .iter()
        .filter(|l| l.name == "deprecated_namespace")
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", out.lints);
    assert_eq!(hits[0].message, "`Old` is deprecated; use Geometry");
}

// --- 6. names an emit-only slot carries -------------------------------------

/// `impl Undefined as end` emitted `do end` and said nothing; with a
/// body the checker reported inside it, not on the head.
#[test]
fn an_impl_on_a_name_that_is_nowhere_reports() {
    let hits = messages("impl Undefined as end\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("nothing declares `Undefined`"), "{hits:?}");

    // A struct of the file, an engine class, and a primitive all pass.
    clean(
        "struct S as\n    x: number\nend\n\nimpl S as\n    function get(self): number\n        return self.x\n    end\nend\n\nlocal s = new S { x = 1 }\n\nprint(s:get())\n",
    );
    let foreign = messages(
        "export impl Vector3 as\n    function flat(self): Vector3\n        return self\n    end\nend\n",
    );
    assert!(foreign.is_empty(), "{foreign:?}");
}

/// A bound is erased in the emit, so nothing checked the name in it.
#[test]
fn a_bound_that_names_nothing_reports() {
    let hits = messages("local function f<T: Undefined>() end\n\nprint(f)\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("`Undefined` names no trait or interface; a bound needs one"),
        "{hits:?}"
    );

    // A trait of the file and a std bound both pass.
    let ok = messages(
        "trait Shape as\n    function area(self): number\nend\n\nlocal function f<T: Shape>(v: T): number\n    return v:area()\nend\n\nlocal function g<T: Clone>(v: T): T\n    return v\nend\n\nprint(f, g)\n",
    );
    assert!(ok.is_empty(), "{ok:?}");
}

/// A method of a field's name overwrote the field with nothing said.
#[test]
fn a_field_and_a_method_of_one_name_report() {
    let hits = messages(
        "struct Circle as\n    radius: number\n    diameter: number\nend\n\nimpl Circle as\n    function diameter(self): number\n        return self.radius * 2\n    end\nend\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("`diameter` is a field of `Circle` and a method of its impl"),
        "{hits:?}"
    );
}

/// `try` on a value that is no Result had no report of its own.
#[test]
fn try_on_a_value_that_is_no_result_reports() {
    let hits = messages(
        "local function f(): number\n    return 1\nend\n\nlocal x: number = try f()\n\nprint(x)\n",
    );
    assert!(
        hits.iter()
            .any(|m| m == "`try` needs a Result; `f()` is `number`"),
        "{hits:?}"
    );
}

/// A struct has one shape, so a pattern over one covers it. The payload
/// of a variant reads the same way.
#[test]
fn a_struct_pattern_covers_its_shape() {
    clean(
        "struct Point as\n    x: number\n    y: number\nend\n\nlocal p = new Point { x = 1, y = 2 }\nlocal r = match p with\n    case Point { x, y } then x + y\nend\n\nprint(r)\n",
    );
    clean(
        "struct Damage as\n    amount: number\nend\n\nenum Hit as\n    Critical(Damage)\n    Normal(Damage)\nend\n\nlocal hit = Hit.Normal(new Damage { amount = 1 })\nlocal r = match hit with\n    case Critical(Damage { amount }) then amount\n    case Normal(d) then d.amount\nend\n\nprint(r)\n",
    );
}

/// `class` has no lowering yet. The render reported it and blanked the
/// block, but the walk never reached the statement, so the raw source
/// shipped into the `.luau` and `build` called it a success.
#[test]
fn a_class_reports_once_and_leaves_no_text() {
    let src = "class Critter\n    public hp: number\n    function heal(self) end\nend\n";
    let (ship, check, messages) = compile(src);

    assert_eq!(
        messages,
        vec![
            "`class` is parsed and not compiled yet; a `struct` with an `impl` is the form that runs"
                .to_string()
        ]
    );

    for out in [&ship, &check] {
        assert!(out.trim().is_empty(), "{out:?}");
        assert_eq!(out.lines().count(), src.lines().count());
    }
}

/// `declare class Name ... end` is the spelling Luau's own definition
/// parser dropped. One statement it cannot read costs the whole
/// definitions file, so every declaration beside it stopped reaching
/// the checker.
#[test]
fn a_declare_class_takes_the_spelling_luau_reads() {
    let options = alloy::EmitOptions {
        file_name: "decl.d.aly".to_string(),
        definitions: true,
        ..alloy::EmitOptions::default()
    };
    let src = "declare function greet(name: string): string\ndeclare version: number\ndeclare class Widget extends Instance\n    Label: string\nend\n";
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.check.lines().count(), src.lines().count());
    assert!(
        out.check
            .contains("declare extern type Widget extends Instance with"),
        "{}",
        out.check
    );
    assert!(!out.check.contains("declare class"), "{}", out.check);
    // A declaration beside it is untouched.
    assert!(
        out.check
            .contains("declare function greet(name: string): string"),
        "{}",
        out.check
    );

    // No `extends`, and the spelling Luau reads already, both hold.
    let plain =
        alloy::compile_with("declare class Widget\n    Label: string\nend\n", &options).unwrap();
    assert!(
        plain.check.contains("declare extern type Widget with"),
        "{}",
        plain.check
    );
    let already = alloy::compile_with(
        "declare extern type Widget with\n    Label: string\nend\n",
        &options,
    )
    .unwrap();
    assert!(
        already.check.contains("declare extern type Widget with"),
        "{}",
        already.check
    );
}

/// Arm order said whether a struct match covered. A specific arm in
/// front of a general one reported a match that every value reaches.
#[test]
fn a_struct_match_covers_in_either_arm_order() {
    let specific_first = "struct Point as\n    x: number\n    y: number\nend\n\nlocal function describe(pt: Point): string\n    return match pt with\n        case Point { x = 0, y = 0 } then \"origin\"\n        case Point { x = x, y = y } then `({x}, {y})`\n    end\nend\n\nprint(describe)\n";
    let general_first = "struct Point as\n    x: number\n    y: number\nend\n\nlocal function describe(pt: Point): string\n    return match pt with\n        case Point { x = x, y = y } then `({x}, {y})`\n        case Point { x = 0, y = 0 } then \"origin\"\n    end\nend\n\nprint(describe)\n";
    clean(specific_first);
    clean(general_first);
}

/// Literal fields in every arm cover nothing, so the report stands.
#[test]
fn a_struct_match_of_literals_alone_is_not_exhaustive() {
    let hits = messages(
        "struct Point as\n    x: number\n    y: number\nend\n\nlocal function describe(pt: Point): string\n    return match pt with\n        case Point { x = 0, y = 0 } then \"origin\"\n        case Point { x = 1, y = 1 } then \"one\"\n    end\nend\n\nprint(describe)\n",
    );
    assert!(
        hits.iter().any(|m| m.contains("not exhaustive")),
        "{hits:?}"
    );
}

/// A `.d.aly` returns no module, so the namespace table had to become an
/// ambient name; its `declare` members leaked into the bare scope.
#[test]
fn a_definitions_namespace_declares_itself_and_keeps_its_members() {
    let options = alloy::EmitOptions {
        file_name: "e.d.aly".to_string(),
        definitions: true,
        ..alloy::EmitOptions::default()
    };
    let src = "export type Profile = { id: number }\n\nexport namespace Store as\n    declare function get_id(profile: Profile): number\n    public const VERSION = 1\nend\n";
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.check.lines().count(), src.lines().count());
    assert!(
        out.check.contains("declare Store: typeof(Store)"),
        "{}",
        out.check
    );
    // The member is a local slot of its own, never a bare declaration.
    assert!(
        out.check
            .contains("local Store_get_id: (profile: Profile) -> number"),
        "{}",
        out.check
    );
    assert!(
        !out.check.contains("declare function get_id"),
        "{}",
        out.check
    );
    assert!(
        out.check.contains("Store.get_id = Store_get_id"),
        "{}",
        out.check
    );
}
