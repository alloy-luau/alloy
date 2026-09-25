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

/// `M.G[]` is an array edit that starts on the namespace path, so the
/// array owns the text and the path renders inside it.
#[test]
fn a_namespace_type_in_an_array_renders_under_one_name() {
    let out = clean(
        "namespace M as\n    struct G as\n        x: number\n    end\nend\n\nlocal gs: M.G[] = [new M.G { x = 1 }]\n\nprint(#gs)\n",
    );
    assert!(out.contains("local gs: __alloy.Array<M_G>"), "{out}");
    assert!(!out.contains("M_G[]"), "{out}");
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

/// `impl Greeter for Widget` inside the namespace that declares the
/// trait reads the contract under the namespace's name, and a default
/// method flattens in from the table the file binds.
#[test]
fn an_impl_of_a_sibling_trait_reads_its_contract() {
    let src = "namespace Group as\n    trait Greeter as\n        function hello(self): string\n        function wave(self): string\n            return \"wave\"\n        end\n    end\n\n    struct Widget as\n        n: number\n    end\n\n    impl Greeter for Widget as\n        function hello(self): string\n            return \"hi\"\n        end\n    end\nend\n";
    let out = clean(src);
    assert!(
        out.contains("Group_Widget.wave = Group_Greeter.wave"),
        "{out}"
    );

    // The contract still holds: a method the impl skips reports.
    let hits = messages(&src.replace(
        "        function hello(self): string\n            return \"hi\"\n        end\n",
        "",
    ));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("`impl Group.Greeter for Group.Widget` does not write `hello`"),
        "{hits:?}"
    );
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

/// An attribute above a member leaves the visibility word to the
/// lead: `@native public function f` writes `local function`, where
/// `public` would be a Luau syntax error. The word goes whatever the
/// declaration under it is.
#[test]
fn an_attributed_member_drops_its_visibility_word() {
    let src = "namespace M as\n    @native\n    public function pub_a(n: number): number\n        return n\n    end\n    @inline\n    private function priv_b(n: number): number\n        return n\n    end\n    @native\n    public local function loc_c(n: number): number\n        return n\n    end\nend\n\nprint(M.pub_a(1), M.loc_c(1))\n";
    let out = clean(src);

    assert!(!out.contains("public"), "{out}");
    assert!(!out.contains("private"), "{out}");
    assert!(
        out.contains("@native local function M_pub_a(n: number)"),
        "{out}"
    );
    assert!(out.contains("local function M_priv_b(n: number)"), "{out}");
    assert!(
        out.contains("@native local function M_loc_c(n: number)"),
        "{out}"
    );
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
        vec!["`M` is already a namespace on line 1; one name holds one declaration".to_string()]
    );
}

/// `global` inside a namespace reports the removal, the way it does
/// anywhere else.
#[test]
fn a_global_inside_a_namespace_reports() {
    let hits = messages("namespace M as\n    global const a = 1\nend\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].starts_with("`global` is removed;"), "{hits:?}");
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

/// A nested namespace of another file names its types at any depth. The
/// module exports one flat name, `Geom_In_Point`, and every type slot of
/// the importing file reads that name: a parameter, a return type, a
/// `local`, and a generic argument.
#[test]
fn a_nested_namespace_of_another_file_folds_at_any_depth() {
    let dir = temp_project("export-nested");
    fs::write(
        dir.join("src/geom.aly"),
        "export namespace Geom as\n    namespace In as\n        struct Point as\n            x: number\n        end\n        namespace Deep as\n            struct Spot as\n                y: number\n            end\n        end\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import { Geom } from \"./geom\"\n\nfunction near(p: Geom.In.Point): Geom.In.Point\n    local q: Geom.In.Point = p\n    return q\nend\n\nfunction deep(xs: Array<Geom.In.Deep.Spot>): number\n    return #xs\nend\n\nprint(near, deep)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let main = output(&dir, "main.luau");
    assert!(
        main.contains("function near(p: Geom_In_Point): Geom_In_Point"),
        "{main}"
    );
    assert!(main.contains("local q: Geom_In_Point = p"), "{main}");
    assert!(
        main.contains("Array<Geom_In_Deep_Spot>") && !main.contains("Geom.In."),
        "{main}"
    );
}

/// `import * as M` binds the module table, so a namespace of the module
/// reads one level deeper: `M.Geom.Point`. The module exports the type
/// as `Geom_Point`, and `M.Geom_Point` is the Luau path a type slot
/// reads. The star name carries it, so `import * as X` reads
/// `X.Geom_Point`.
#[test]
fn a_star_import_folds_a_namespace_type() {
    let dir = temp_project("star-namespace");
    fs::write(
        dir.join("src/geom.aly"),
        "export namespace Geom as\n    struct Point as\n        x: number\n    end\n    enum Kind as\n        Flat\n        Round\n    end\n    namespace In as\n        namespace Deep as\n            struct Spot as\n                y: number\n            end\n        end\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import * as M from \"./geom\"\nimport * as X from \"./geom\"\n\nlocal p: M.Geom.Point = { x = 1 }\nlocal k: M.Geom.Kind = M.Geom.Kind.Flat\nlocal s: M.Geom.In.Deep.Spot = { y = 2 }\nlocal q: X.Geom.Point = p\n\nprint(p, k, s, q)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let main = output(&dir, "main.luau");
    assert!(main.contains("local p: M.Geom_Point"), "{main}");
    assert!(main.contains("local k: M.Geom_Kind"), "{main}");
    assert!(main.contains("local s: M.Geom_In_Deep_Spot"), "{main}");
    assert!(main.contains("local q: X.Geom_Point"), "{main}");
}

/// `import type { A }` of a namespace writes one alias per member. A
/// namespace is no type of its own: the module exports `A_Shape`, so
/// `type A = _m1.A` names nothing. A plain type in the same list keeps
/// its own alias.
#[test]
fn a_type_only_import_writes_the_namespace_aliases() {
    let dir = temp_project("type-only-namespace");
    fs::write(
        dir.join("src/decl.aly"),
        "export namespace A as\n    struct Shape as\n        n: number\n    end\n    namespace B as\n        struct Deep as\n            m: number\n        end\n    end\nend\n\nexport type Meters = number\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import type { A, Meters } from \"./decl\"\n\nlocal function use(s: A.Shape, d: A.B.Deep, m: Meters): number\n    return s.n + d.m + m\nend\n\nprint(use)\n",
    )
    .unwrap();
    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let main = output(&dir, "main.luau");
    assert!(main.contains("type A_Shape = _m1.A_Shape"), "{main}");
    assert!(main.contains("type A_B_Deep = _m1.A_B_Deep"), "{main}");
    assert!(main.contains("type Meters = _m1.Meters"), "{main}");
    assert!(!main.contains("type A = _m1.A"), "{main}");
    assert!(
        main.contains("function use(s: A_Shape, d: A_B_Deep, m: Meters)"),
        "{main}"
    );
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

// --- 5b. a compile-time member by its path ----------------------------------

/// `@M.tag(3)` outside the namespace reads the attribute the group
/// declares. The attach table keys the data by the attribute's own
/// name, so `Attributes.get` reads one key whichever way a use spells
/// it.
#[test]
fn an_attribute_reads_by_its_path() {
    let out = clean(
        "namespace M as\n    attribute tag(n: number) on function\nend\n\n@M.tag(3)\nfunction f()\nend\n\nprint(f)\n",
    );
    assert!(out.contains("__alloy.attach(f, { tag = { 3 } })"), "{out}");
}

/// `@M.tag` with no arguments, and a nested path.
#[test]
fn a_nested_attribute_reads_by_its_path() {
    let out = clean(
        "namespace Outer as\n    namespace Inner as\n        attribute deep on function\n    end\nend\n\n@Outer.Inner.deep\nfunction f()\nend\n\nprint(f)\n",
    );
    assert!(out.contains("__alloy.attach(f, { deep = {  } })"), "{out}");
}

/// The target, the argument types and the contract all read through a
/// path, the way they read a bare name.
#[test]
fn an_attribute_path_takes_every_check() {
    let ns = "namespace M as\n    attribute tag(n: number) on function\nend\n\n";
    let target = messages(&format!("{ns}@M.tag(1)\nstruct S as\n    x: number\nend\n"));
    assert_eq!(target.len(), 1, "{target:?}");
    assert!(
        target[0].contains("`M.tag` has no meaning on a struct"),
        "{target:?}"
    );

    let args = messages(&format!(
        "{ns}@M.tag(\"x\")\nfunction f()\nend\n\nprint(f)\n"
    ));
    assert_eq!(args.len(), 1, "{args:?}");
    assert!(
        args[0].contains("takes number for `n`, string given"),
        "{args:?}"
    );
}

/// `$M.twice(2)` expands in place, and the expansion keeps the line
/// count, which `clean` reads.
#[test]
fn a_macro_expands_by_its_path() {
    let out = clean(
        "namespace M as\n    macro twice(x)\n        x * 2\n    end\nend\n\nprint($M.twice(2))\n",
    );
    assert!(out.contains("print((2 * 2))"), "{out}");

    let nested = clean(
        "namespace Outer as\n    namespace Inner as\n        macro plus(x)\n            x + 1\n        end\n    end\nend\n\nprint($Outer.Inner.plus(4))\n",
    );
    assert!(nested.contains("print((4 + 1))"), "{nested}");
}

/// A path that names no namespace, and a namespace with no such
/// member: one report, naming the path.
#[test]
fn a_path_that_reaches_nothing_reports() {
    let head = messages("@Nope.tag\nfunction f()\nend\n\nprint(f)\n");
    assert_eq!(head.len(), 1, "{head:?}");
    assert_eq!(
        head[0],
        "`Nope` is no namespace, so `Nope.tag` names no attribute"
    );

    let member = messages(
        "namespace M as\n    attribute tag on function\nend\n\n@M.nope\nfunction f()\nend\n\nprint(f)\n",
    );
    assert_eq!(member.len(), 1, "{member:?}");
    assert_eq!(member[0], "`M` declares no attribute `nope`");

    let mac = messages(
        "namespace M as\n    macro twice(x)\n        x * 2\n    end\nend\n\nprint($M.nope(1))\n",
    );
    assert_eq!(mac.len(), 1, "{mac:?}");
    assert_eq!(mac[0], "`M` declares no macro `nope`");

    let bare = messages("print($Nope.thing(1))\n");
    assert_eq!(bare.len(), 1, "{bare:?}");
    assert_eq!(
        bare[0],
        "`Nope` is no namespace, so `$Nope.thing` names no macro"
    );
}

/// A private member reached from outside says it is private, not that
/// it is unknown.
#[test]
fn a_private_compile_time_member_reads_as_private() {
    let attr = messages(
        "namespace M as\n    private attribute tag on function\nend\n\n@M.tag\nfunction f()\nend\n\nprint(f)\n",
    );
    assert_eq!(attr.len(), 1, "{attr:?}");
    assert_eq!(attr[0], "`tag` is private to `M`");

    let mac = messages(
        "namespace M as\n    private macro twice(x)\n        x * 2\n    end\nend\n\nprint($M.twice(2))\n",
    );
    assert_eq!(mac.len(), 1, "{mac:?}");
    assert_eq!(mac[0], "`twice` is private to `M`");
}

/// A member and a name of the file that share a name stay two
/// declarations: the body reads the member, the file reads its own.
#[test]
fn a_member_name_beats_a_file_name_inside_the_body() {
    let out = clean(concat!(
        "attribute tag(outer: string) on function\n",
        "macro shout(x)\n    print(\"outer\", x)\nend\n\n",
        "namespace M as\n",
        "    attribute tag(inner: number) on function\n",
        "    macro shout(x)\n        print(\"inner\", x)\n    end\n\n",
        "    @tag(3)\n    public function m()\n    end\n\n",
        "    public function go()\n        $shout(\"a\")\n    end\n",
        "end\n\n",
        "@tag(\"s\")\nfunction top()\nend\n\n",
        "$shout(\"b\")\nprint(M.m, M.go, top)\n",
    ));
    assert!(out.contains("print(\"inner\", \"a\")"), "{out}");
    assert!(out.contains("print(\"outer\", \"b\")"), "{out}");
    assert!(
        out.contains("__alloy.attach(M_m, { tag = { 3 } })"),
        "{out}"
    );
    assert!(
        out.contains("__alloy.attach(top, { tag = { \"s\" } })"),
        "{out}"
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

/// A macro is source, not a value, so it takes no name of its own and
/// the table carries none.
#[test]
fn a_macro_member_keeps_its_own_name() {
    let out = clean(
        "namespace M as\n    macro twice(x)\n        x * 2\n    end\n    attribute tag(name: string) on field\nend\n\nprint($M.twice(2))\n",
    );
    assert!(out.contains("print((2 * 2))"), "{out}");
    assert!(!out.contains("M.twice"), "{out}");
}

/// A member of a namespace reads by its path from outside. The bare
/// name reaches none, and the report names every namespace that
/// declares it, so the reader writes the path.
#[test]
fn a_bare_name_names_no_member_from_outside() {
    let src = "namespace A as\n    macro twice(x)\n        x * 2\n    end\nend\n\nnamespace B as\n    macro twice(x)\n        x * 3\n    end\nend\n\nprint($twice(2))\n";
    assert_eq!(
        messages(src),
        vec!["`twice` is a macro of `A` and `B`; write `$A.twice` or `$B.twice`"]
    );

    let attr = "namespace A as\n    attribute tag on function\nend\n\n@tag\nfunction f()\nend\n";
    assert_eq!(
        messages(attr),
        vec!["`tag` is an attribute of `A`; write `@A.tag`"]
    );
}

/// An attribute is a value the runtime reads, so it renders under the
/// namespace's name and lands on the table. The name it records stays
/// the one the source wrote, so `Attributes.get` reads one key.
#[test]
fn an_attribute_member_takes_the_namespace_name() {
    let out =
        clean("namespace M as\n    attribute tag(name: string) on function\nend\n\nprint(M.tag)\n");
    assert!(
        out.contains("local M_tag = __alloy.attribute(\"tag\", { \"function\" }, { \"name\" })"),
        "{out}"
    );
    assert!(out.contains("M.tag = M_tag"), "{out}");
}

/// A private attribute stays a local: the table never carries it, the
/// way a private function goes.
#[test]
fn a_private_attribute_member_stays_off_the_table() {
    let out = clean(
        "namespace M as\n    private attribute tag on function\n\n    @tag\n    public function f()\n    end\nend\n\nprint(M.f)\n",
    );
    assert!(out.contains("local M_tag = __alloy.attribute"), "{out}");
    assert!(!out.contains("M.tag = "), "{out}");
}

/// The attach line names the function the emit wrote, `M_f`. It named
/// the source name, which binds nothing, and the checker called it an
/// unknown global.
#[test]
fn an_attribute_attaches_to_the_rendered_function() {
    let out = clean(
        "namespace M as\n    attribute tag(n: number) on function\n\n    @tag(3)\n    public function f()\n    end\nend\n\nprint(M.f)\n",
    );
    assert!(
        out.contains("__alloy.attach(M_f, { tag = { 3 } })"),
        "{out}"
    );
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

// --- an impl of a namespace member -------------------------------------------

const ZOO: &str = "export namespace Zoo as\n    struct Lion as\n        read name: string\n        private roar_power: number = 10\n    end\nend\n\nimpl Zoo.Lion as\n    function roar(self): string\n        return self.name\n    end\n\n    private function secret(self): number\n        return self.roar_power\n    end\nend\n";

/// A struct of a namespace renders under one name, and its impl reads
/// that name: `self` takes the struct's type, and a private method lands
/// on the private table. `self` used to be unknown, so every read of a
/// field through it failed to type.
#[test]
fn an_impl_of_a_namespace_struct_types_self_by_the_rendered_name() {
    let options = alloy::EmitOptions {
        check: true,
        file_name: "t.aly".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with(ZOO, &options).unwrap();

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.check.contains("function Zoo_Lion.roar(self: Zoo_Lion)"),
        "{}",
        out.check
    );
    assert!(
        out.check
            .contains("function Zoo_Lion__private.secret(self: Zoo_Lion__all)"),
        "{}",
        out.check
    );

    // The ship artifact writes on the class table the file binds.
    assert!(
        ship(ZOO).contains("function Zoo_Lion.roar("),
        "{}",
        ship(ZOO)
    );
}

/// The indexes a cross-file impl travels through are keyed by the
/// rendered name. A dotted target used to travel nowhere, so the
/// declaring file's check artifact never declared the method and the
/// checker called the write an added property.
#[test]
fn a_cross_file_impl_of_a_namespace_struct_reaches_the_class_table() {
    let ext = "import { Zoo } from \"./ns\"\n\nimpl Zoo.Lion as\n    function roar_louder(self): number\n        return self.roar_power * 2\n    end\nend\n";
    let project = alloy::extensions::project_impls(&[ZOO.to_string(), ext.to_string()]);
    let travelled: Vec<&str> = project
        .methods
        .iter()
        .filter(|m| m.target == "Zoo_Lion")
        .map(|m| m.name.as_str())
        .collect();

    assert_eq!(travelled, vec!["roar_louder"]);

    // The full view travels under the same name, so the impl types
    // `self` as it and reaches the private field.
    assert!(
        alloy::extensions::private_views(ZOO).contains(&"Zoo_Lion".to_string()),
        "{:?}",
        alloy::extensions::private_views(ZOO)
    );

    // The declaring file's own impl stays its own: only its private
    // method travels, for the privacy lint.
    assert_eq!(
        project.privates,
        vec![("Zoo_Lion".to_string(), vec!["secret".to_string()])]
    );
}

/// A namespace inside a namespace joins both names. The impl of a member
/// two levels down reads the same rendered name, `A_B_S`.
#[test]
fn an_impl_two_namespaces_deep_reads_one_name() {
    let src = "export namespace A as\n    namespace B as\n        struct S as\n            read tag: string\n            private hidden: number = 1\n        end\n    end\nend\n\nimpl A.B.S as\n    function show(self): string\n        return self.tag\n    end\nend\n";
    let options = alloy::EmitOptions {
        check: true,
        file_name: "t.aly".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with(src, &options).unwrap();

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.check.contains("function A_B_S.show(self: A_B_S)"),
        "{}",
        out.check
    );
    assert!(
        alloy::extensions::private_views(src).contains(&"A_B_S".to_string()),
        "{:?}",
        alloy::extensions::private_views(src)
    );
}

/// A struct inside a namespace is checked through every path that
/// names it. The unknown-field report and the `private_access` lint
/// read one shape, and an importing module writes `Zoo.Box`, so the
/// shape has to carry that name beside the emitted `Zoo_Box`.
#[test]
fn the_field_checks_reach_a_namespace_struct_by_its_path() {
    let dir = temp_project("ns-fields");
    fs::write(
        dir.join("src/zoo.aly"),
        "export namespace Zoo as\n    struct Box as\n        x: number\n        private secret: number = 0\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/imported.aly"),
        "import { Zoo } from \"./zoo\"\n\nlocal _b = new Zoo.Box { x = 1, bad = 2, secret = 5 }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/samefile.aly"),
        "namespace Zoo as\n    struct Box as\n        x: number\n        private secret: number = 0\n    end\nend\n\nlocal _b = new Zoo.Box { x = 1, secret = 5 }\n",
    )
    .unwrap();
    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::check_project(&dir, &config).unwrap();
    let errors: Vec<String> = report
        .diagnostics
        .iter()
        .map(|(_, d)| d.message.clone())
        .collect();
    assert_eq!(
        errors
            .iter()
            .filter(|m| m.contains("no field `bad`"))
            .count(),
        1,
        "{errors:?}"
    );

    let private: Vec<(String, String)> = report
        .lints
        .iter()
        .filter(|(_, l)| l.name == "private_access")
        .map(|(p, l)| (p.display().to_string(), l.message.clone()))
        .collect();
    assert_eq!(private.len(), 2, "{private:?}");
    assert!(
        private.iter().any(|(p, _)| p.contains("imported")),
        "{private:?}"
    );
    assert!(
        private.iter().any(|(p, _)| p.contains("samefile")),
        "{private:?}"
    );
}

/// A method named `new` in a namespace struct's own impl is the user's
/// function. `new Zoo.Lion { }` builds the struct, so it calls the raw
/// constructor the declaration writes. The emit read the written name,
/// `Zoo.Lion`, which no index of a struct holds, so the construction
/// fell through to `Zoo.Lion.new` and the checker typed it against the
/// user's signature.
#[test]
fn a_construction_reads_the_raw_constructor_not_a_user_new() {
    let src = "namespace Zoo as\n    struct Lion as\n        name: string\n    end\n\n    impl Lion as\n        function new(name: string): Lion\n            return new Zoo.Lion { name = name }\n        end\n    end\nend\n\nprint(Zoo)\n";
    let (ship, check, messages) = compile(src);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(
        check.contains("return Zoo_Lion.__new({ name = name })"),
        "{check}"
    );
    assert!(ship.contains("return Zoo_Lion({ name = name })"), "{ship}");
}

/// The bare name inside the namespace names the same member the path
/// names, so `new B { }` and `new NS.B { }` build the struct the same
/// way. The bare name resolved to no struct index, so the construction
/// fell through to `NS_B.new` and the checker typed the fields table
/// against the user's parameters.
#[test]
fn a_bare_construction_inside_the_namespace_reads_the_raw_constructor() {
    let head =
        "namespace NS as\n    struct B as\n        x: number\n        y: number\n    end\n\n";
    let user_new = format!(
        "{head}    impl B as\n        function new(x: number, y: number): B\n            return new B {{ x = x, y = y }}\n        end\n    end\nend\n\nprint(NS)\n"
    );
    let (ship, check, messages) = compile(&user_new);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(
        check.contains("return NS_B.__new({ x = x, y = y })"),
        "{check}"
    );
    assert!(ship.contains("return NS_B({ x = x, y = y })"), "{ship}");

    // The qualified path takes the same name.
    let qualified = user_new.replace("new B {", "new NS.B {");
    let (_, check, messages) = compile(&qualified);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(
        check.contains("return NS_B.__new({ x = x, y = y })"),
        "{check}"
    );

    // Two levels deep: the bare name inside the inner namespace names
    // the member `Out_In_B`.
    let nested = "namespace Out as\n    namespace In as\n        struct B as\n            x: number\n        end\n\n        impl B as\n            function new(x: number): B\n                return new B { x = x }\n            end\n        end\n    end\nend\n\nprint(Out)\n";
    let (ship, check, messages) = compile(nested);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(
        check.contains("return Out_In_B.__new({ x = x })"),
        "{check}"
    );
    assert!(ship.contains("return Out_In_B({ x = x })"), "{ship}");

    // No user `new`: the construction reads the raw constructor already,
    // and the bare name still resolves to the member.
    let plain = format!(
        "{head}    function make(): B\n        return new B {{ x = 1, y = 2 }}\n    end\nend\n\nprint(NS)\n"
    );
    let (ship, check, messages) = compile(&plain);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(
        check.contains("return NS_B.__new({ x = 1, y = 2 })"),
        "{check}"
    );
    assert!(ship.contains("return NS_B({ x = 1, y = 2 })"), "{ship}");

    // The field check reads the same struct, so a name the struct lacks
    // reports from the bare form too, under the name the source wrote.
    let bad = user_new.replace("{ x = x, y = y }", "{ x = x, z = y }");
    let (_, _, messages) = compile(&bad);
    assert!(
        messages.iter().any(|m| m.contains("`B` has no field `z`")),
        "{messages:?}"
    );
}

/// Inside its namespace an enum reads by its own name in a `match`, as
/// it does in a value: the arms resolve, and the tests name the rendered
/// enum. A missing arm names the enum by its path.
#[test]
fn a_match_inside_a_namespace_reads_the_enum_by_its_own_name() {
    let src = "namespace N as\n    public enum Kind as\n        A\n        B(number)\n    end\n    public function f(k: Kind): number\n        return match k with\n            case Kind.A then 1\n            case Kind.B(n) then n\n        end\n    end\nend\nprint(N.f(N.Kind.A))\n";
    let out = alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.ship.contains("== N_Kind.A"), "{}", out.ship);
    assert!(!out.ship.contains("== Kind.A"), "{}", out.ship);

    let missing = src.replace("            case Kind.B(n) then n\n", "");
    let out = alloy::compile_with(&missing, &alloy::EmitOptions::default()).unwrap();
    let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        ["this match is not exhaustive: `N.Kind` has no arm for `B`; add it or a `default` arm"]
    );
}

/// A `local` member is one variable. A write inside the namespace
/// reaches `Game.count`, and a write to `Game.count` reaches the body.
#[test]
fn a_local_member_is_one_variable() {
    let src = "namespace Game as\n    const MAX = 3\n    local count = 0\n    function add(): number\n        count += 1\n        return count\n    end\n    function read(): number\n        return count\n    end\nend\nGame.add()\nGame.add()\nprint(Game.count, Game.MAX)\nGame.count = 10\nprint(Game.read(), Game.add(), Game.count)\n";
    let (ship, check, messages) = compile(src);
    assert!(messages.is_empty(), "{messages:?}");
    // The check artifact keeps the copy, which types the field.
    assert!(check.contains("Game.count = Game_count"), "{check}");
    assert!(check.contains("Game.MAX = Game_MAX"), "{check}");

    let dir = std::env::temp_dir().join(format!("alloy-ns-local-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("alloy.luau"), alloy::RUNTIME).unwrap();
    fs::write(
        dir.join(".luaurc"),
        "{ \"aliases\": { \"alloy\": \"./alloy\" } }\n",
    )
    .unwrap();
    fs::write(dir.join("main.luau"), &ship).unwrap();
    let run = std::process::Command::new("luau")
        .arg("main.luau")
        .current_dir(&dir)
        .output();
    let _ = fs::remove_dir_all(&dir);

    let Ok(run) = run else {
        eprintln!("skipped: luau is not installed");

        return;
    };
    let out =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);

    assert_eq!(out.trim(), "2\t3\n10\t11\t11", "{out}\n{ship}");
}

/// A struct or an enum of a namespace crosses a remote with the layout a
/// top-level one gets. The layout read `Combat.Hit` as no type of the
/// file: the spec had no wire, an array item read `any`, and `@u8` did
/// nothing. A sibling that a field names by its own name reads too.
#[test]
fn a_namespace_type_crosses_a_remote_with_its_layout() {
    let src = "namespace Combat\n    struct Hit\n        @u8 damage: number\n        kind: Kind\n    end\n    enum Kind\n        Slash\n        Burn(number)\n    end\nend\nremote Land(hit: Combat.Hit, rows: Combat.Hit[], k: Combat.Kind) from client\n";
    let (ship, _, messages) = compile(src);
    assert!(messages.is_empty(), "{messages:?}");

    let kind =
        "{ enum = Combat_Kind, tags = { Slash = 0, Burn = 1 }, slots = { Burn = { \"f64\" } } }";
    let hit = format!(
        "{{ fields = {{ {{ \"damage\", \"u8\" }}, {{ \"kind\", {kind} }} }}, struct = Combat_Hit }}"
    );
    let wire = format!("wire = {{ {hit}, {{ item = {hit}, array = true }}, {kind} }}");
    assert!(ship.contains(&wire), "{wire}\n{ship}");
}

/// A namespace of another module: the layout reads the member in the
/// module that declares it, through a named import and a star import,
/// and that module registers the table a field names.
#[test]
fn an_imported_namespace_type_crosses_a_remote_with_its_layout() {
    let dir = temp_project("wire");
    fs::write(
        dir.join("src/net.aly"),
        "export namespace Combat\n    struct Hit\n        @u8 damage: number\n        kind: Kind\n    end\n    enum Kind\n        Slash\n        Burn(number)\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/use.aly"),
        "import { Combat } from \"./net\"\nimport * as Net from \"./net\"\nexport remote Land(hit: Combat.Hit) from client\nexport remote Star(hit: Net.Combat.Hit) from client\nprint(Combat.Kind.Burn(1) == Combat.Kind.Burn(1))\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.diagnostics.is_empty(), "{report:?}");

    let read = |file: &str| fs::read_to_string(dir.join("out").join(file)).unwrap();
    let used = read("use.luau");
    let kind = "{ enum = \"net.aly:Combat_Kind\", tags = { Slash = 0, Burn = 1 }, slots = { Burn = { \"f64\" } } }";

    for path in ["Combat.Hit", "Net.Combat.Hit"] {
        let layout = format!(
            "wire = {{ {{ fields = {{ {{ \"damage\", \"u8\" }}, {{ \"kind\", {kind} }} }}, struct = {path} }} }}"
        );
        assert!(used.contains(&layout), "{layout}\n{used}");
    }

    assert!(
        read("net.luau").contains("wire.types[\"net.aly:Combat_Kind\"] = Combat_Kind"),
        "{}",
        read("net.luau")
    );

    // The enum of an imported namespace compares by identity too.
    assert!(
        report
            .lints
            .iter()
            .any(|(_, l)| l.name == "identity_compare" && l.message.contains("`Combat.Kind`")),
        "{:?}",
        report.lints
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `new N.X.Z { }` with `X` a struct names nothing. The path resolved
/// to `N_X` and dropped `Z`, so the build made an `N.X` in silence, and
/// empty braces said "leaves `a` unset". The path now reaches the
/// checker whole, which reports "`N.X` has no struct `Z`".
#[test]
fn a_path_past_a_struct_names_no_struct() {
    let src = "namespace N\n    struct X\n        a: number\n    end\nend\nprint(new N.X.Z { a = 1 }, new N.X.Z {})\n";
    let (ship, _, messages) = compile(src);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(ship.contains("construct(N.X.Z, { a = 1 })"), "{ship}");
    assert!(!ship.contains("N_X({"), "{ship}");
}

/// A trait of a namespace bounds a generic: by its own name inside the
/// namespace, and by its path outside. Inside, the bound read no trait;
/// outside, the check artifact wrote `T & Zoo.Named`, a type path Luau
/// does not have.
#[test]
fn a_namespace_trait_bounds_a_generic() {
    let src = "namespace Zoo\n    trait Named\n        function name(self): string\n    end\n    function first<T: Named>(xs: T[]): string\n        return xs[1]:name()\n    end\nend\nstruct Cat\n    n: string\nend\nimpl Zoo.Named for Cat\n    function name(self): string\n        return self.n\n    end\nend\nstruct Rock\n    w: number\nend\nlocal function outer<T: Zoo.Named>(x: T): string\n    return x:name()\nend\nprint(Zoo.first, outer(new Cat { n = \"c\" }), outer(new Rock { w = 1 }))\n";
    let (_, check, messages) = compile(src);
    assert_eq!(
        messages,
        ["`Rock` does not implement `Zoo.Named`; `outer` asks for it"]
    );
    assert!(check.contains("(xs[1] :: (T & Zoo_Named))"), "{check}");
    assert!(check.contains("outer<T>(x: (T & Zoo_Named))"), "{check}");
}
