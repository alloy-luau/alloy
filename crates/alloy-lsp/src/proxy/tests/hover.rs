use super::super::hover::{
    child_cast, child_value_home, impl_self_type, intrinsic_code_home, member_doc, shadow_home,
    shadows_an_import, source_type, star_module_hover,
};
use super::super::*;
use super::support::one_file;

#[test]
pub(crate) fn a_declared_annotation_keeps_its_type_arguments() {
    let src = "local damaged: Signal<Player, number> = Signal.new()\nlocal n: number = 1\nlocal function f(a: { x: number, y: number }, b: string) end\n";
    assert_eq!(
        declared_annotation(src, "damaged", 6).map(|(_, a)| a),
        Some("Signal<Player, number>".to_string())
    );
    assert_eq!(
        declared_annotation(src, "n", 60).map(|(_, a)| a),
        Some("number".to_string())
    );
    assert_eq!(
        declared_annotation(src, "a", 90).map(|(_, a)| a),
        Some("{ x: number, y: number }".to_string())
    );
}
/// A hover reads the binding the caret can see: the parameter of
/// another function with the same name annotates nothing here, and a
/// `local` the source gave no type keeps the child's own answer.
#[test]
pub(crate) fn a_hover_keeps_the_annotation_of_the_binding_in_scope() {
    let src = concat!(
        "function useit(format: string): string\n",
        "    return format\n",
        "end\n",
        "\n",
        "function loopit()\n",
        "    local format = 1\n",
        "    print(format)\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let counted = "```luau\nlocal format: number\n```";

    // The `local` of `loopit` and its use: the child's `number` stands.
    assert_eq!(keep_annotation(counted, doc, 5, 10), None);
    assert_eq!(keep_annotation(counted, doc, 6, 10), None);

    // The parameter, in the function that declares it.
    assert_eq!(
        keep_annotation("```luau\nlocal format: unknown\n```", doc, 1, 11),
        Some("```luau\nlocal format: string\n```".to_string())
    );
}

/// A state with one open document, so the declarations are there.
pub(crate) fn hover_of(src: &str, line: u32, character: u32, printed: &str) -> String {
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut text = format!("```luau\n{printed}\n```");

    for step in [
        declared_signature as fn(&str, &Doc, u32, u32) -> Option<String>,
        name_trait_method,
        name_by_declaration,
        unlocal_parameter,
    ] {
        if let Some(next) = step(&text, doc, line, character) {
            text = next;
        }
    }

    if let Some(next) = name_method_receiver(&text, doc, line) {
        text = next;
    }

    if let Some(next) = restore_struct_arguments(&text, doc, line) {
        text = next;
    }

    if let Some(next) = drop_bound_intersections(&text, doc) {
        text = next;
    }

    text.trim_start_matches("```luau\n")
        .trim_end_matches("\n```")
        .to_string()
}
#[test]
pub(crate) fn a_bound_reads_where_the_source_wrote_it() {
    let src = concat!(
        "export trait Priced as\n",
        "    function price(self): number\n",
        "end\n",
        "\n",
        "export function cheapest<T: Priced>(a: T, b: T): T\n",
        "    return a\n",
        "end\n",
    );

    assert_eq!(
        hover_of(
            src,
            4,
            17,
            "export function cheapest<T>(a: Priced & T, b: Priced & T): T"
        ),
        "export function cheapest<T: Priced>(a: T, b: T): T"
    );
}
/// A bound reaches a local through the cast the check artifact writes.
/// The artifact puts `(T & Ord)` on the array's element and on the read
/// of one, so the trait's record prints twice, and the two assignments
/// print the branch twice. The fold names the record, drops the
/// repeats, and the hover and the hint both read `T`.
#[test]
pub(crate) fn a_bound_on_a_local_reads_as_the_parameter() {
    let src = concat!(
        "trait Ord as\n",
        "    function cmp(self, other: number): number\n",
        "end\n",
        "\n",
        "function largestBoxed<T: Ord>(xs: (T & Ord)[]): T\n",
        "    local best = xs[1]\n",
        "    return best\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let record = "{\n    read cmp: (self: any, other: number) -> number\n}";
    let branch = format!("(T & {record} & {record})");
    let printed = format!("```luau\nlocal best: {branch} | {branch}\n```");
    let mut text = alloy::shapes::fold(&printed, &st.known_shapes_at(Some(uri)));

    assert_eq!(text, "```luau\nlocal best: (T & Ord)\n```");

    if let Some(next) = drop_bound_intersections(&text, doc) {
        text = next;
    }

    assert_eq!(text, "```luau\nlocal best: T\n```");

    // The hint on the same binding reads the same way.
    let mut hints = vec![json!({
        "position": { "line": 5, "character": 14 },
        "label": ": (T & Ord)",
        "textEdits": [{ "newText": ": (T & Ord)" }],
    })];
    clean_hints(&mut hints, doc);

    assert_eq!(hints[0]["label"], json!(": T"));
    assert_eq!(hints[0]["textEdits"][0]["newText"], json!(": T"));
}

/// A hint that spells out how the runtime lays out an enum names
/// nothing the source wrote. The hover names the type, and the gutter
/// stays empty.
#[test]
fn a_hint_of_a_runtime_layout_goes() {
    let src = "import { Err } from \"./errors\"\nconst short = missing(1)\nprint(short)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut hints = vec![json!({
        "position": { "line": 1, "character": 11 },
        "label": ": (\"Full\" | { _1: \"Junk\" | { @metatable Item, { _1: number } }, _2: number })?",
    })];
    clean_hints(&mut hints, doc);

    assert!(hints.is_empty(), "{hints:?}");
}
#[test]
pub(crate) fn a_union_keeps_the_order_the_source_wrote() {
    let src = concat!(
        "export function describe_any(v: string | number | boolean): string\n",
        "    return \"x\"\n",
        "end\n",
    );

    assert_eq!(
        hover_of(
            src,
            0,
            17,
            "export function describe_any(v: boolean | number | string): string"
        ),
        "export function describe_any(v: string | number | boolean): string"
    );
}
#[test]
pub(crate) fn a_generic_struct_keeps_its_arguments() {
    let src = concat!(
        "export struct Slotted<T> as\n",
        "    value: T\n",
        "end\n",
        "\n",
        "impl Slotted<T> as\n",
        "    function get(self): T\n",
        "        return self.value\n",
        "    end\n",
        "end\n",
    );

    assert_eq!(
        hover_of(src, 5, 14, "function Slotted.get<T>(self: Slotted): T"),
        "function Slotted.get<T>(self: Slotted<T>): T"
    );
    assert_eq!(
        hover_of(src, 5, 18, "local self: Slotted"),
        "self: Slotted<T>\n```\nA parameter of `function get`."
    );
}
#[test]
pub(crate) fn a_trait_method_reads_with_its_name_and_its_receiver() {
    let src = concat!(
        "export trait Describable as\n",
        "    function label(self): string\n",
        "end\n",
    );

    assert_eq!(
        hover_of(src, 1, 14, "function (self: any): string"),
        "function Describable.label(self: Describable): string"
    );
    assert_eq!(
        hover_of(src, 1, 14, "function x:label(self: any): string"),
        "function Describable:label(self: Describable): string"
    );
}
#[test]
pub(crate) fn a_parameter_hover_keeps_a_record_type_whole() {
    let src = concat!(
        "local function Stat(props: { label: string, name: string }): number\n",
        "    return 1\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let start = src.find("props").expect("props");
    let answer = declared_parameter_hover(doc, start, start + "props".len()).expect("hover");

    assert!(
        answer.starts_with("```alloy\nprops: { label: string, name: string }\n```"),
        "{answer}"
    );
    assert!(
        answer.ends_with("A parameter of `function Stat`."),
        "{answer}"
    );

    // The `>` of an arrow closes no bracket.
    let arrows = "local function Button(props: { on_click: () -> () })\n    return 1\nend\n";
    let (st, uri) = one_file(arrows);
    let doc = st.docs.get(uri).expect("doc");
    let start = arrows.find("props").expect("props");
    let answer = declared_parameter_hover(doc, start, start + "props".len()).expect("hover");

    assert!(
        answer.starts_with("```alloy\nprops: { on_click: () -> () }\n```"),
        "{answer}"
    );
}
#[test]
pub(crate) fn a_foreign_impl_names_its_type() {
    let src = concat!(
        "export impl string as\n",
        "    function trim(self): string\n",
        "        return self\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let at = src.find("trim").expect("trim");

    assert_eq!(
        foreign_method_hover(doc, at, at + "trim".len()),
        Some("```alloy\nfunction string.trim(self: string): string\n```".to_string())
    );

    // A struct's own impl reads through the child.
    let own = concat!(
        "struct Item as\n",
        "    id: number\n",
        "end\n",
        "impl Item as\n",
        "    function room(self): number\n",
        "        return self.id\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(own);
    let doc = st.docs.get(uri).expect("doc");
    let at = own.find("room").expect("room");

    assert_eq!(foreign_method_hover(doc, at, at + "room".len()), None);
}
#[test]
pub(crate) fn a_parameter_is_not_a_local() {
    let src = "export function room(count: number): number\n    return count\nend\n";

    assert_eq!(
        hover_of(src, 1, 12, "local count: number"),
        "count: number\n```\nA parameter of `function room`."
    );
    // A name the file declares with a keyword keeps its keyword.
    let bound = "local total = 1\nprint(total)\n";
    assert_eq!(
        hover_of(bound, 1, 7, "local total: number"),
        "local total: number"
    );
}
#[test]
pub(crate) fn a_binding_reads_the_type_its_call_declares() {
    let src = concat!(
        "export function checked(n: number): Result<number, string>[]\n",
        "    return []\n",
        "end\n",
        "\n",
        "local rows = checked(1)\n",
    );

    assert_eq!(
        hover_of(src, 4, 7, "local rows: t3"),
        "local rows: Result<number, string>[]"
    );
}
/// A default import and an `import * as` hover as the module: the
/// import line and the public names, not the module's table.
#[test]
pub(crate) fn a_module_import_hovers_as_the_module() {
    let dir = std::env::temp_dir().join(format!("alloy-module-hover-{}", std::process::id()));
    let pkg = dir.join("packages");
    std::fs::create_dir_all(&pkg).expect("temp dir");
    std::fs::write(
            pkg.join("fluid.luau"),
            "local m = {}\nm.__SCHEDULER_INTERFACE = {}\nfunction m.create(x) return x end\nm.mount = 1\nreturn m\n",
        )
        .expect("module");
    let src = "import fluid from \"@pkg/fluid\"\nimport { create } from \"@pkg/fluid\"\nimport * as f2 from \"./packages/fluid\"\nprint(fluid, create, f2)\n";
    let from = dir.join("main.aly");
    let aliases = vec![("pkg".to_string(), pkg.clone())];
    // On the binding the child's table stands.
    assert_eq!(
        module_hover(src, "fluid", Some(&from), &aliases, false),
        None
    );

    // On the path the answer is the import line and nothing else:
    // the document link on the same characters offers to follow it.
    let hover = module_hover(src, "fluid", Some(&from), &aliases, true).expect("a path hover");
    assert_eq!(hover, "```alloy\nimport fluid from \"@pkg/fluid\"\n```");

    // The alias segment answers the same import line.
    let by_alias = module_hover(src, "pkg", Some(&from), &aliases, true).expect("an alias hover");
    assert_eq!(by_alias, hover);

    // A path that names no file is the child's to answer.
    assert_eq!(module_hover(src, "gone", Some(&from), &aliases, true), None);

    std::fs::remove_dir_all(&dir).ok();
}

/// A star alias of an Alloy module hovers as a std star alias does:
/// the import line and the names the module exports. The child printed
/// the module's table, with its types as `t7`.
#[test]
fn a_star_alias_of_an_alloy_module_hovers_as_the_module() {
    let dir = std::env::temp_dir().join(format!("alloy-star-hover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("types.aly"),
        "export struct Blade as damage: number end\nexport enum Hit\n    Miss\nend\nexport type Id = number\nlocal hidden = 1\n",
    )
    .expect("module");
    std::fs::write(dir.join("lib.luau"), "return { a = 1 }\n").expect("module");
    let src = "import * as Ty from \"./types\"\nimport * as L from \"./lib\"\nprint(Ty.Hit.Miss, x.Ty, L)\n";
    let from = dir.join("main.aly");
    let hover = |needle: &str, word: &str| {
        let start = src.find(needle).expect("the name");

        star_module_hover(src, word, start, Some(&from), &[])
    };

    assert_eq!(
        hover("Ty from", "Ty").as_deref(),
        Some("```alloy\nimport * as Ty from \"./types\"\n```\nExports: `Blade`, `Hit`, `Id`")
    );
    assert_eq!(hover("Ty.Hit", "Ty"), hover("Ty from", "Ty"));
    // A member of another name, and a Luau module, keep the child's answer.
    assert_eq!(hover("Ty, L", "Ty"), None);
    assert_eq!(hover("L)", "L"), None);

    std::fs::remove_dir_all(&dir).ok();
}

/// `@self/x` names `x` in the folder of an `init` script, as the
/// compiler reads it, so its star alias hovers as the module. Any other
/// file has no `@self`, and the child answers there.
#[test]
fn a_star_alias_of_a_self_import_hovers_in_an_init_script() {
    let dir = std::env::temp_dir().join(format!("alloy-self-hover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("a.aly"), "export const A = 1\n").expect("module");
    let src = "import * as M from '@self/a'\n";
    let hover = |file: &str| star_module_hover(src, "M", 12, Some(&dir.join(file)), &[]);

    assert_eq!(
        hover("init.server.aly").as_deref(),
        Some("```alloy\nimport * as M from '@self/a'\n```\nExports: `A`")
    );
    assert_eq!(hover("main.aly"), None);

    std::fs::remove_dir_all(&dir).ok();
}

/// A name the module passes on with `export { T } from` is one of its
/// exports. The list left it out. Its spec reads from the module's own
/// folder.
#[test]
fn a_star_alias_lists_a_name_passed_on_with_export_from() {
    let dir = std::env::temp_dir().join(format!("alloy-pass-hover-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("game")).expect("temp dir");
    std::fs::write(
        dir.join("game/scored.aly"),
        "export type Tally = number\nexport trait Scored\n  function score(self): number\nend\n",
    )
    .expect("module");
    std::fs::write(
        dir.join("game/scoring.aly"),
        "export { Scored, Tally as Count } from './scored'\nexport const BONUS = 2\n",
    )
    .expect("module");
    let src = "import * as Game from './game/scoring'\n";

    assert_eq!(
        star_module_hover(src, "Game", 12, Some(&dir.join("main.aly")), &[]).as_deref(),
        Some(
            "```alloy\nimport * as Game from './game/scoring'\n```\nExports: `BONUS`, `Scored`, `Count`"
        )
    );

    std::fs::remove_dir_all(&dir).ok();
}
/// A hover on a std member reads the member's own section, not the
/// type's whole page. The receiver resolves from the source: an
/// annotation, an initializer, or the type name itself.
#[test]
pub(crate) fn a_hover_on_a_std_member_names_the_member() {
    let src = concat!(
        "local prices: HashMap<string, number> = HashMap.new()\n",
        "local price = prices:get(\"sword\")\n",
        "local xs = [ 1, 2, 3 ]\n",
        "local n = xs:len()\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");

    let at_new = std_member_hover("```luau\n(...)\n```", doc, 0, 49).expect("HashMap.new");
    assert!(at_new.starts_with("**HashMap.new**"), "{at_new}");

    let at_get = std_member_hover("```luau\n(...)\n```", doc, 1, 22).expect("HashMap:get");
    assert!(at_get.starts_with("**HashMap:get**"), "{at_get}");
    assert!(
        at_get.contains("```alloy"),
        "the section carries an example"
    );

    let at_len = std_member_hover("```luau\n(...)\n```", doc, 3, 14).expect("Array:len");
    assert!(at_len.starts_with("**Array:len**"), "{at_len}");
}
/// With no annotation the type the child printed names the receiver.
#[test]
pub(crate) fn a_printed_type_names_the_member_the_source_cannot() {
    let src = "local n = whatever:pop()\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "```luau\n(self: Queue<string>) -> string?\n```";
    let hover = std_member_hover(printed, doc, 0, 20).expect("Queue:pop");

    assert!(hover.starts_with("**Queue:pop**"), "{hover}");
}
/// A hover on the type name keeps the overview and lists the names.
#[test]
pub(crate) fn a_hover_on_a_std_type_lists_its_members() {
    let text = alloy::docs::type_markdown("HashMap").expect("HashMap");

    assert!(text.contains("A map with methods"), "the overview stays");
    assert!(text.contains("Members: `new`, `from`, `get`"), "{text}");
    assert!(!text.contains("|---|"), "no table is left");
}
/// A match lowers to one expression, so a `case` binding has no
/// local; the pattern says what it holds.
#[test]
pub(crate) fn a_case_binding_reads_its_payload() {
    const SRC: &str = "struct Boost as\n    stat: string\n    amount: number\nend\n\nenum Effect as\n    Heal(number)\n    Buff(Boost)\nend\n\nlocal function s(e: Effect): number\n    return match e with\n        case Heal(n) then n\n        case Buff(b) then b.amount\n    end\nend\nprint(s)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let known = st.known_shapes_at(Some(uri));
    let at = |needle: &str| SRC.find(needle).expect("needle");
    let line_of = |o: usize| position_of(SRC, o).0 as usize;
    let heal = at("case Heal(n) then n") + "case Heal(".len();
    let used = at("then n\n") + "then ".len();
    let bound = at("case Buff(b)") + "case Buff(".len();
    let field = at("b.amount") + "b.".len();

    assert_eq!(
        case_binding_text(doc, line_of(heal), heal, "n", &known),
        Some("```alloy\nn: number\n```\nA binding of `Effect.Heal`.".to_string())
    );
    assert_eq!(
        case_binding_text(doc, line_of(used), used, "n", &known),
        Some("```alloy\nn: number\n```\nA binding of `Effect.Heal`.".to_string())
    );
    assert_eq!(
        case_binding_text(doc, line_of(bound), bound, "b", &known),
        Some("```alloy\nb: Boost\n```\nA binding of `Effect.Buff`.".to_string())
    );
    assert_eq!(
        case_binding_text(doc, line_of(field), field, "amount", &known),
        Some("```alloy\namount: number\n```\nA field of `struct Boost`.".to_string())
    );
}
/// A name a nested pattern binds reads its type off the level that
/// binds it: a variant's payload, with the arguments of a generic one
/// from the payload above, a struct's field, and an `or` the union of
/// its sides. The emit writes the path in the name's place, so the child
/// answered for the text after the name.
#[test]
pub(crate) fn a_nested_case_binding_reads_its_payload() {
    const SRC: &str = "struct Point as\n    x: number\n    y: number\nend\n\nenum Opt<T> as\n    Some(T)\n    Nil\nend\n\nenum Item as\n    Sword(number)\n    Wand(string)\nend\n\nenum Box as\n    It(Item)\n    O(Opt<Item>)\n    Pt(Point)\nend\n\nlocal function s(b: Box): string\n    return match b with\n        case Box.It(Item.Sword(d)) then d:upper()\n        case Box.O(Opt.Some(it)) then tostring(it)\n        case Box.Pt(Point { x = px }) then tostring(px)\n        case Box.It(Item.Sword(n) or Item.Wand(n)) then tostring(n)\n        default \"x\"\n    end\nend\nprint(s)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let known = st.known_shapes_at(Some(uri));
    let hover = |needle: &str, word: &str| {
        let at = SRC.find(needle).expect("needle");

        case_binding_text(doc, position_of(SRC, at).0 as usize, at, word, &known)
    };

    assert_eq!(
        hover("d:upper", "d").as_deref(),
        Some("```alloy\nd: number\n```\nA binding of `Item.Sword`.")
    );
    assert_eq!(
        hover("it)\n", "it").as_deref(),
        Some("```alloy\nit: Item\n```\nA binding of `Opt.Some`.")
    );
    assert_eq!(
        hover("px)\n", "px").as_deref(),
        Some("```alloy\npx: number\n```\nA binding of field `x` of `Point`.")
    );
    assert_eq!(
        hover("n)\n", "n").as_deref(),
        Some("```alloy\nn: number | string\n```\nA binding of `Item.Sword` or `Item.Wand`.")
    );

    // Go to definition lands on the name in the pattern.
    let used = SRC.find("d:upper").expect("use");
    let bound = SRC.find("Sword(d)").expect("pattern") + "Sword(".len();
    assert_eq!(
        case_binding_span(doc, position_of(SRC, used).0 as usize, "d", &known),
        Some((bound, bound + 1))
    );
}

/// A struct pattern that names its struct by a path, `N.P { x }`,
/// reads the fields off the declaration of `P`.
#[test]
fn a_qualified_struct_pattern_types_its_fields() {
    const SRC: &str = "namespace N\n    struct P\n        x: number\n    end\nend\n\nenum Box as\n    Pt(N.P)\n    Empty\nend\n\nlocal function s(b: Box): number\n    return match b with\n        case Box.Pt(N.P { x = px }) then px\n        default 0\n    end\nend\nprint(s)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let known = st.known_shapes_at(Some(uri));
    let at = SRC.find("px }").expect("needle");

    assert_eq!(
        case_binding_text(doc, position_of(SRC, at).0 as usize, at, "px", &known).as_deref(),
        Some("```alloy\npx: number\n```\nA binding of field `x` of `N.P`.")
    );
}

/// A call of a name a `case` pattern binds: the child names the call
/// after the path the emit writes, `Item._1`. The label takes the name.
#[test]
fn a_call_of_a_case_binding_names_the_binding() {
    let src = "enum Item as\n    Wand((n: number) -> string)\n    Nothing\nend\nlocal function f(i: Item): string\n    return match i with\n        case Item.Wand(w) then w(1)\n        default \"x\"\n    end\nend\nprint(f)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut result = json!({
        "signatures": [{
            "label": "function Item._1(n: number): string",
            "parameters": [{ "label": [17, 26] }],
        }],
    });
    restyle_signatures(&mut result, doc, 6, 34);
    assert_eq!(
        result["signatures"][0]["label"],
        "function w(n: number): string"
    );
}

/// The `default` arm binds nothing, so a name in it is not the payload
/// a sibling `case` bound. The emit gives the arm no shadow, and the
/// child answers with the outer local.
#[test]
pub(crate) fn a_name_in_the_default_arm_is_no_case_binding() {
    const SRC: &str = "enum Msg as\n    Join(string)\nend\n\nlocal name = \"outer\"\nlocal function s(m: Msg): string\n    match m with\n        case Join(name) then\n            return `shadowed: {name}`\n        default\n            return `outer: {name}`\n    end\nend\nprint(s, name)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let known = st.known_shapes_at(Some(uri));
    let shadowed = SRC.find("shadowed: {name}").expect("arm") + "shadowed: {".len();
    let outer = SRC.find("outer: {name}").expect("default") + "outer: {".len();
    let line_of = |o: usize| position_of(SRC, o).0 as usize;

    assert_eq!(
        case_binding_text(doc, line_of(shadowed), shadowed, "name", &known),
        Some("```alloy\nname: string\n```\nA binding of `Msg.Join`.".to_string())
    );
    assert_eq!(
        case_binding_text(doc, line_of(outer), outer, "name", &known),
        None
    );
}
/// A record field of a `type` body hovers as the line declares it.
/// The child sees a table key and answers with an unnamed function
/// type, which says nothing about the field.
#[test]
pub(crate) fn a_type_body_field_reads_as_it_is_written() {
    const SRC: &str = "export type HudProps = {\n    on_swing: () -> (),\n    label: string,\n}\nprint(nil :: HudProps)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let at = SRC.find("on_swing").expect("on_swing");

    assert_eq!(
        declared_field_hover(doc, at, at + "on_swing".len()),
        Some("```alloy\non_swing: () -> ()\n```\nA field of `type HudProps`.".to_string())
    );
    let label = SRC.find("label").expect("label");

    assert_eq!(
        declared_field_hover(doc, label, label + "label".len()),
        Some("```alloy\nlabel: string\n```\nA field of `type HudProps`.".to_string())
    );
}
/// A field of a namespace member names the path the source writes.
/// The declaration index keys the member twice, under `Ns.T` and under
/// the `Ns_T` the emit writes, and the walk took the emit's name.
#[test]
pub(crate) fn a_namespace_member_field_names_the_path() {
    const SRC: &str =
        "export namespace Ns as\n    struct T as\n        value: number,\n    end\nend\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let at = SRC.find("value").expect("value");

    assert_eq!(
        declared_field_hover(doc, at, at + "value".len()),
        Some("```alloy\nvalue: number\n```\nA field of `struct Ns.T`.".to_string())
    );
}
/// A `type` that names no record has no field to answer for, and a
/// name below the closed body belongs to nothing.
#[test]
pub(crate) fn a_field_hover_stops_at_the_end_of_the_body() {
    const SRC: &str =
        "type Id = number\ntype Props = {\n    a: number,\n}\nlocal b: number = 1\nprint(b)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let at = SRC.rfind("b: number").expect("b");

    assert_eq!(declared_field_hover(doc, at, at + 1), None);
}
/// The key of a struct's raw constructor names the field, past the
/// visibility the declaration writes.
#[test]
pub(crate) fn a_field_key_reads_past_its_visibility() {
    assert_eq!(field_key("    public read id: number"), Some("id"));
    assert_eq!(field_key("    write notes: string = \"\""), Some("notes"));
    assert_eq!(field_key("end"), None);
}
/// A remote's parameter reads as the line declares it; the child
/// measures the string key the emit writes for it.
#[test]
pub(crate) fn a_remote_parameter_reads_as_it_is_written() {
    const SRC: &str = "export remote PickUp(@u32 id: number, @u8 count: number) from client\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let at = SRC.find("id:").expect("id");

    assert_eq!(
        remote_parameter_hover(doc, at, at + 2),
        Some("```alloy\n@u32 id: number\n```\nA parameter of `remote PickUp`.".to_string())
    );
    let second = SRC.find("count:").expect("count");

    assert_eq!(
        remote_parameter_hover(doc, second, second + 5),
        Some("```alloy\n@u8 count: number\n```\nA parameter of `remote PickUp`.".to_string())
    );
}
#[test]
pub(crate) fn a_byte_count_is_the_keys_own_text() {
    assert!(is_byte_count("```alloy\nstring (5 bytes)\n```"));
    assert!(is_byte_count("```luau\nstring (1 byte)\n```"));
    assert!(!is_byte_count("```alloy\nstring\n```"));
}
/// A hover that restates the token under the cursor says nothing.
#[test]
pub(crate) fn a_type_alias_to_itself_is_no_hover() {
    assert!(restates_itself("```alloy\ntype Player = Player\n```"));
    assert!(restates_itself("```alloy\ntype keyof<T> = keyof<T>\n```"));
    assert!(!restates_itself(
        "```alloy\ntype Profile = { name: string }\n```"
    ));
}
/// `print(undefined_var)`: the child answers `type undefined_var =
/// unknown`, which names a type the file has not got and the caret is
/// not on. A hover that invents one says nothing.
#[test]
pub(crate) fn an_unknown_name_hovers_to_nothing() {
    let (st, uri) = super::support::one_file("print(undefined_var)\n");
    let doc = st.docs.get(uri).expect("doc");
    assert!(invents_a_type(
        "```alloy\ntype undefined_var = unknown\n```",
        doc
    ));
    assert!(invents_a_type("```alloy\ntype gone = any\n```", doc));
    assert!(!invents_a_type(
        "```alloy\ntype Point = { x: number }\n```",
        doc
    ));

    // The file's own alias to `unknown` still reads.
    let (st, uri) = super::support::one_file("export type Thing = unknown\nlocal t: Thing = 1\n");
    let doc = st.docs.get(uri).expect("doc");
    assert!(!invents_a_type("```alloy\ntype Thing = unknown\n```", doc));
}

#[test]
pub(crate) fn a_new_name_after_a_declaring_keyword_completes_to_nothing() {
    let src = "enum Col\nlocal x = fo\nfunction hud(a\nimport x from \"./x\"\nprint(x)\n";
    assert!(declares_a_name_at(src, 8));
    assert!(declares_a_name_at(src, 6));
    assert!(!declares_a_name_at(src, 21));
    assert!(!declares_a_name_at(src, 36));
    assert!(declares_a_name_at(src, 45));
    assert!(!declares_a_name_at(src, src.len() - 2));

    // `namespace` names one too: `namespace Na|me` drew the whole scope.
    let src = "namespace Name as end\n";
    assert!(declares_a_name_at(src, 12));
    assert!(declares_a_name_at(src, 10));
}
#[test]
pub(crate) fn a_doc_the_child_read_is_not_added_again() {
    let src =
        "--- HUD Component\nexport function Hud(props: number): number\n    return props\nend\n";
    let doc = Doc::new(
        src.to_string(),
        1,
        &EmitOptions::default(),
        &alloy::luaux::Config::default(),
        None,
    );

    // The shadow keeps the comment, so the child's hover carries it.
    let from_child = "```luau\nfunction Hud(props: number): number\n```\n----------\nHUD Component";
    let restyled = restyle_hover(from_child, &doc, 1, 17).expect("restyled");
    assert_eq!(restyled.matches("HUD Component").count(), 1);
    assert!(restyled.starts_with("```alloy\nexport function Hud("));

    // A hover without the doc gets it from the binding.
    let bare = "```luau\nfunction Hud(props: number): number\n```";
    let restyled = restyle_hover(bare, &doc, 1, 17).expect("restyled");
    assert_eq!(restyled.matches("HUD Component").count(), 1);
}
#[test]
pub(crate) fn a_declared_attribute_names_its_targets_in_the_hover() {
    let hover = "```alloy\n@icon(asset: string)\n```\n\n**Applies to** `struct` · `enum`";
    assert_eq!(declared_attribute_targets(hover), vec!["struct", "enum"]);
    assert!(declared_attribute_targets("```alloy\nlocal x\n```").is_empty());
}
#[test]
pub(crate) fn a_method_arity_leaves_out_the_receiver() {
    assert_eq!(
        without_self("Argument count mismatch. Function expects 1 argument, but 3 are specified")
            .as_deref(),
        Some("Argument count mismatch. Function expects 0 arguments, but 2 are specified")
    );
    assert_eq!(
        without_self(
            "Argument count mismatch. Function expects 3 arguments, but only 2 are specified"
        )
        .as_deref(),
        Some("Argument count mismatch. Function expects 2 arguments, but only 1 is specified")
    );
}
#[test]
pub(crate) fn a_method_finds_the_impl_that_writes_it() {
    let source = "impl Counter as\n    function bump(self): number\n        return 1\n    end\n\n    function make(): Counter\n    end\nend\n";
    assert_eq!(method_owner(source, "bump").as_deref(), Some("Counter"));
    assert_eq!(method_owner(source, "make"), None);
}

/// A private field read inside its impl. The child prints the type
/// alone; the declaration carries `private` and the struct.
#[test]
fn a_field_at_a_use_reads_its_declaration() {
    let src = "struct S as\n    x: number\n    private secret: number\nend\n\nimpl S as\n    function f(self): number\n        return self.secret + self.x\n    end\nend\n\nlocal s = new S { x = 1, secret = 2 }\n\nprint(s.x)\nprint(s?.x)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let at = |needle: &str| {
        let start = src.find(needle).expect("needle");

        (start, start + needle.len())
    };
    let (start, end) = at("secret + ");
    let (start, end) = (start, end - " + ".len());
    assert_eq!(
        used_field_hover(&st, doc, start, end).as_deref(),
        Some("```alloy\nprivate secret: number\n```\nA field of `struct S`.")
    );

    // A local bound by a constructor names the struct too.
    let (start, end) = at("s.x)");
    let (start, end) = (start + 2, end - 1);
    assert_eq!(
        used_field_hover(&st, doc, start, end).as_deref(),
        Some("```alloy\nx: number\n```\nA field of `struct S`.")
    );

    // `?.` reads the same field.
    let (start, end) = at("s?.x)");
    let (start, end) = (start + 3, end - 1);
    assert_eq!(
        used_field_hover(&st, doc, start, end).as_deref(),
        Some("```alloy\nx: number\n```\nA field of `struct S`.")
    );

    // A method after a `:` is no field.
    assert_eq!(used_field_hover(&st, doc, 0, 1), None);
}

#[test]
fn every_hop_of_a_field_chain_names_its_owner() {
    let a = "import { B } from \"./b\"\n\nstruct A as\n    b: B,\nend\n\nlocal a = new A { b = new B { c = new C { value = 1 } } }\nprint(a.b.c.value)\n";
    let b = "export struct B as\n    c: C,\nend\n";
    let c = "export struct C as\n    value: number,\nend\n";
    let st = super::support::files(&[
        ("file:///a.aly", a),
        ("file:///b.aly", b),
        ("file:///c.aly", c),
    ]);
    let doc = st.docs.get("file:///a.aly").expect("doc");
    // The name of one hop, by the text that runs from it.
    let hover = |rest: &str, name: usize| {
        let start = a.rfind(rest).expect("the chain");

        used_field_hover(&st, doc, start, start + name)
    };
    assert_eq!(
        hover("b.c.value", 1).as_deref(),
        Some("```alloy\nb: B\n```\nA field of `struct A`.")
    );
    assert_eq!(
        hover("c.value", 1).as_deref(),
        Some("```alloy\nc: C\n```\nA field of `struct B`.")
    );
    assert_eq!(
        hover("value)", 5).as_deref(),
        Some("```alloy\nvalue: number\n```\nA field of `struct C`.")
    );
}

/// The second link of a chain named the receiver by the first word of
/// the folded self type, `read`.
#[test]
fn a_chain_link_names_the_receiver_type() {
    let known = alloy::shapes::Known::default();
    let text = "function w.xs:map(function(x: number) return x end):find(self: {read number}, f: (number, number) -> boolean): number?";
    assert_eq!(
        alloy::shapes::fold(text, &known),
        "function Array:find(self: read number[], f: (number, number) -> boolean): number?"
    );
}

/// The bracket of `x?[k]` reads `T?`: the guard answers nil, and the
/// child sees only the index inside it. `x![k]` keeps `T`, and a plain
/// index keeps whatever the child said.
#[test]
fn a_guarded_index_reads_the_element_type() {
    let src = concat!(
        "type P = { name: string }\n",
        "local mo: { [string]: P }? = nil\n",
        "local m: { [string]: P } = {}\n",
        "local a = mo?[\"k\"]\nlocal b = mo![\"k\"]\nlocal c = m[\"k\"]\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).unwrap();
    let text = "```alloy\nP\n```";
    let column = |line: u32| {
        doc.source
            .lines()
            .nth(line as usize)
            .and_then(|l| l.find('['))
            .expect("a bracket") as u32
    };

    assert_eq!(
        optional_index_hover(text, doc, 3, column(3)).as_deref(),
        Some("```alloy\nP?\n```")
    );
    assert_eq!(optional_index_hover(text, doc, 4, column(4)), None);
    assert_eq!(optional_index_hover(text, doc, 5, column(5)), None);

    // A type that already answers nil takes no second `?`.
    assert_eq!(
        optional_index_hover("```alloy\nP?\n```", doc, 3, column(3)),
        None
    );
}

/// Luau's solver gives a function it infers from a definition with no
/// parameters a variadic tail, so a module's table printed
/// `default: (...any) -> string` beside `add: (a: number, b: number)`.
/// The declaration says the list is empty.
#[test]
fn a_function_with_no_parameters_closes_its_pack() {
    let src = concat!(
        "export function add(a: number, b: number): number\n",
        "    return a + b\n",
        "end\n",
        "\n",
        "export async function fetchName(): string\n",
        "    return \"alice\"\n",
        "end\n",
        "\n",
        "export namespace Geo as\n",
        "    function origin(): number\n",
        "        return 0\n",
        "    end\n",
        "end\n",
        "\n",
        "export default function makeDefault(): string\n",
        "    return \"d\"\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = &st.docs[uri];
    let empty = empty_parameter_names(doc);

    assert!(empty.contains("fetchName"));
    assert!(empty.contains("origin"));
    // `export default function makeDefault` binds `default` in the
    // module's table, which is the key the reader sees.
    assert!(empty.contains("default"));
    assert!(empty.contains("makeDefault"));
    assert!(!empty.contains("add"));

    let printed = concat!(
        "local M: {\n",
        "    Geo: {\n",
        "        origin: (...any) -> number\n",
        "    },\n",
        "    add: (...any) -> number,\n",
        "    default: (...any) -> string,\n",
        "    fetchName: (...any) -> Future<string>\n",
        "}",
    );

    assert_eq!(
        close_empty_packs(printed, &empty),
        concat!(
            "local M: {\n",
            "    Geo: {\n",
            "        origin: () -> number\n",
            "    },\n",
            // A print with more names than the source wrote belongs to
            // another function; `add` takes two parameters.
            "    add: (...any) -> number,\n",
            "    default: () -> string,\n",
            "    fetchName: () -> Future<string>\n",
            "}",
        )
    );
}

/// A written `(...any)` is a function that really takes anything. The
/// std types carry several, and none of them closes.
#[test]
fn a_declared_variadic_keeps_its_pack() {
    let (st, uri) = one_file("export function takes(...: any): number\n    return 1\nend\n");
    let doc = &st.docs[uri];
    let empty = empty_parameter_names(doc);
    let printed = "local M: {\n    takes: (...any) -> number\n}";

    assert!(!empty.contains("takes"));
    assert_eq!(close_empty_packs(printed, &empty), printed);
}

/// A `local` inside a function shadows an import of the same name. The
/// hover read the import's declaration at the local's own line and at
/// every use of it.
#[test]
fn a_local_shadows_the_import_it_hides() {
    let src = concat!(
        "import { shared } from \"./src\"\n",
        "\n",
        "local function useShared(): string\n",
        "    local shared = \"local-shadow\"\n",
        "\n",
        "    return shared\n",
        "end\n",
        "\n",
        "print(shared, useShared())\n",
    );
    let at = |needle: &str| src.find(needle).expect("the word");

    // The local's own line, and the use under it.
    assert!(shadows_an_import(src, "shared", at("shared = ")));
    assert!(shadows_an_import(src, "shared", at("return shared") + 7));
    // The import list itself, and the use at the top level, where the
    // local is out of scope.
    assert!(!shadows_an_import(src, "shared", at("shared }")));
    assert!(!shadows_an_import(src, "shared", at("shared, useShared")));
}

/// A parameter and a `for` variable shadow an import the same way.
#[test]
fn a_parameter_and_a_loop_variable_shadow_too() {
    let src = concat!(
        "import { shared } from \"./src\"\n",
        "\n",
        "local function f(shared: string): string\n",
        "    return shared\n",
        "end\n",
        "\n",
        "for shared in pairs({}) do\n",
        "    print(shared)\n",
        "end\n",
    );
    let at = |needle: &str| src.find(needle).expect("the word");

    assert!(shadows_an_import(src, "shared", at("shared: string")));
    assert!(shadows_an_import(src, "shared", at("return shared") + 7));
    assert!(shadows_an_import(src, "shared", at("shared in pairs")));
    assert!(shadows_an_import(src, "shared", at("print(shared)") + 6));
}

/// Inside a trait's own default method `self` is whichever type
/// implements the trait. The trait itself is what the reader can name,
/// and the print was `any`.
#[test]
pub(crate) fn self_inside_a_trait_default_method_names_the_trait() {
    let src = concat!(
        "trait Shape as\n",
        "    function area(self): number\n",
        "\n",
        "    function describe(self): string\n",
        "        return `area {self:area()}`\n",
        "    end\n",
        "end\n",
        "\n",
        "struct Circle as\n",
        "    radius: number\n",
        "end\n",
        "\n",
        "impl Shape for Circle as\n",
        "    function area(self): number\n",
        "        return self.radius\n",
        "    end\n",
        "end\n",
    );

    assert_eq!(
        hover_of(src, 4, 25, "local self: any"),
        "self: Shape\n```\nA parameter of `function describe`."
    );
    // The `impl` below still names the struct it is for.
    assert_eq!(
        hover_of(src, 14, 20, "local self: any"),
        "local self: Circle"
    );
}

/// `import { Thing as ThingAlias }`: the hover on the alias reads the
/// struct the module declares, not the constructor table the child
/// prints under a solver variable.
#[test]
fn an_import_alias_reads_the_export_s_declaration() {
    let st = super::support::files(&[
        (
            "file:///defs.aly",
            "export struct Thing as\n    n: number\nend\n",
        ),
        (
            "file:///use.aly",
            "import { Thing as ThingAlias } from \"./defs\"\nlocal x: ThingAlias = new ThingAlias { n = 1 }\n",
        ),
    ]);
    let doc = st.docs.get("file:///use.aly").expect("doc");
    assert_eq!(
        import_alias_source(&doc.source, "ThingAlias").as_deref(),
        Some("Thing")
    );

    // A name imported under its own spelling keeps to the plain lookup.
    assert_eq!(import_alias_source(&doc.source, "Thing"), None);

    let hover = st
        .docs
        .values()
        .flat_map(|d| d.decls.iter())
        .find(|d| d.name == "Thing")
        .map(|d| d.hover.clone())
        .expect("Thing");
    assert!(hover.contains("export struct Thing\n"), "{hover}");
    assert!(!hover.contains(" where "), "{hover}");
}

/*
`a:map(double)` on `local a: Box<number>`: the child prints the method
as declared, `map<T, U>(self: Box, f: (T) -> U): Box`. Signature help
binds the impl's `T` to the receiver's `number` and keeps the method's
own `U`, with the declared return `Box<U>`. The hint on the binding
reads the argument `double` binds, `Box<number>`, and inserts it.
*/
#[test]
fn a_method_on_an_instantiated_struct_carries_the_instantiation() {
    let src = "struct Box<T> as\n    inner: T\nend\n\nimpl Box<T> as\n    function map<U>(self, f: (T) -> U): Box<U>\n        return new Box { inner = f(self.inner) }\n    end\nend\n\nlocal a: Box<number> = new Box { inner = 1 }\n\nlocal function double(x: number): number\n    return x * 2\nend\n\nlocal mapped = a:map(double)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut result = json!({
        "signatures": [{
            "label": "function Box:map<T, U>(self: Box, f: (T) -> U): Box",
            "parameters": [{ "label": [34, 45] }],
        }],
    });
    restyle_signatures(&mut result, doc, 16, 22);
    assert_eq!(
        result["signatures"][0]["label"],
        "function Box:map<U>(self: Box<number>, f: (number) -> U): Box<U>"
    );
    assert_eq!(
        result["signatures"][0]["parameters"],
        json!([{ "label": "self: Box<number>" }, { "label": "f: (number) -> U" }])
    );

    assert_eq!(source_type(doc, 16, 12).as_deref(), Some("Box<number>"));

    let mut hints = vec![json!({
        "kind": 1,
        "label": ": Box",
        "position": { "line": 16, "character": 12 },
    })];
    clean_hints(&mut hints, doc);
    assert_eq!(hint_label(&hints[0]), ": Box<number>");
    assert_eq!(hints[0]["textEdits"][0]["newText"], ": Box<number>");
}

/*
Alloy spells a return type `function f(): T` and `function f() -> T`,
and the arrow setting makes the hint read like the file. The return
hint stands after the `)` of the parameters; a variable and a
parameter hint stand after a name and keep the colon. The gutter
spaces the label with the hint's padding, and the edit writes a space
of its own.
*/
#[test]
fn the_arrow_setting_reaches_the_return_hint_alone() {
    let src = "local function add(a: number, b: number)\n    return a + b\nend\n\nlocal total = add(1, 2)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let hints = || {
        vec![
            json!({ "kind": 1, "label": ": number", "position": { "line": 0, "character": 40 },
                    "textEdits": [{ "newText": ": number" }] }),
            json!({ "kind": 1, "label": ": number", "position": { "line": 4, "character": 11 },
                    "textEdits": [{ "newText": ": number" }] }),
        ]
    };

    // Off, which is the default: both hints keep the colon.
    let mut colon = hints();
    arrow_returns(&mut colon, doc, false);
    assert_eq!(hint_label(&colon[0]), ": number");
    assert_eq!(hint_label(&colon[1]), ": number");

    let mut arrow = hints();
    arrow_returns(&mut arrow, doc, true);
    assert_eq!(hint_label(&arrow[0]), "-> number");
    assert_eq!(arrow[0]["textEdits"][0]["newText"], " -> number");
    assert_eq!(arrow[0]["paddingLeft"], json!(true));

    // The binding on line 4 takes `local total: number` and nothing
    // else, so its hint is untouched.
    assert_eq!(arrow[1], hints()[1]);
}

/*
`$dbg(Point.new(1))`: the emit writes the argument as a string for the
message and as code after it. A caret inside the inner call maps to
the string, where the child sees no call to help with; the code copy
is where it answers. A call outside an intrinsic maps to code already.
*/
#[test]
fn a_call_inside_an_intrinsic_argument_maps_to_its_code_copy() {
    let src = "struct Point as\n    x: number\nend\n\nimpl Point as\n    function new(x: number): Point\n        return new Point { x = x }\n    end\nend\n\nlocal v1 = $dbg(Point.new(1))\nlocal v2 = wrap(Point.new(1))\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let home = |line: u32, character: u32| {
        intrinsic_code_home(&doc.source, &doc.shadow, line, line, character)
    };
    let (line, column) = home(10, 26).expect("the code copy");
    let text = doc.shadow.lines().nth(line as usize).expect("the line");

    assert!(text[..column as usize].ends_with("Point.new("), "{text}");
    assert!(!crate::context::in_string(text, column as usize), "{text}");
    // The intrinsic's own list, and a call outside one.
    assert_eq!(home(10, 16), None);
    assert_eq!(home(11, 26), None);
}

/*
`a:map(g(1))`: the argument is a call, and the declared return of `g`,
`(number) -> number`, binds the method's `U` the way a named function
does, so the hint on `m` reads `Box<number>`. A call of a function the
file does not declare binds nothing, and the hint stays as the child
printed it.
*/
#[test]
fn a_call_as_the_argument_of_a_generic_method_binds_its_return() {
    let src = "struct Box<T> as\n    inner: T\nend\n\nimpl Box<T> as\n    function map<U>(self, f: (T) -> U): Box<U>\n        return new Box { inner = f(self.inner) }\n    end\nend\n\nfunction g(n: number): (number) -> number\n    return function(x)\n        return x + n\n    end\nend\n\nlocal a: Box<number> = new Box { inner = 1 }\nlocal m = a:map(g(1))\nlocal u = a:map(h(1))\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut hints = vec![
        json!({
            "kind": 1,
            "label": ": Box",
            "position": { "line": 17, "character": 7 },
        }),
        json!({
            "kind": 1,
            "label": ": Box",
            "position": { "line": 18, "character": 7 },
        }),
    ];
    clean_hints(&mut hints, doc);
    assert_eq!(hint_label(&hints[0]), ": Box<number>");
    assert_eq!(hints[0]["textEdits"][0]["newText"], ": Box<number>");
    assert_eq!(hint_label(&hints[1]), ": Box");
}

/*
`local function add(a, b)`: the solver names one type parameter per
untyped parameter, and the letters say nothing. The signature reads as
the source wrote it, the parameter hover drops the letter, and the
gutter holds no hint at all.
*/
#[test]
fn an_untyped_parameter_drops_the_solver_s_letter() {
    let src = "local function add(a, b)\n    return a + b\nend\n";
    let (st, uri) = super::support::one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "```luau\nlocal function add<a, b>(a: a, b: b): add<a, b>\n```";
    assert_eq!(
        declared_signature(printed, doc, 0, 15).as_deref(),
        Some("```luau\nlocal function add(a, b)\n```")
    );

    // The child prints `a` for `b` too; neither letter reaches the
    // reader.
    assert_eq!(
        unlocal_parameter("```luau\nlocal b: a\n```", doc, 0, 22).as_deref(),
        Some("```luau\nb\n```\nA parameter of `function add`.")
    );

    let mut hints = vec![
        json!({ "kind": 1, "label": ": a", "position": { "line": 0, "character": 23 } }),
        json!({ "kind": 1, "label": ": add<a, b>", "position": { "line": 0, "character": 24 } }),
        json!({ "kind": 1, "label": ": number", "position": { "line": 0, "character": 24 } }),
    ];
    clean_hints(&mut hints, doc);

    // A type the solver did name stays.
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(hint_label(&hints[0]), ": number");
}

/// A type the source declares keeps its own letter: `<T>` is the
/// author's word, not the solver's.
#[test]
fn a_declared_generic_keeps_its_signature() {
    let src = "local function first<T>(xs: { T }): T\n    return xs[1]\nend\n";
    let (st, uri) = super::support::one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "```luau\nlocal function first<T>(xs: { T }): T\n```";
    assert_eq!(declared_signature(printed, doc, 0, 21), None);
}

/// A local is bound above its use. The emit of an `enum` at the top of
/// the file binds `v` in a function of its own; the nearest line above
/// the hover wins over the first line of the file.
#[test]
pub(crate) fn a_hover_home_takes_the_nearest_line_above() {
    let shadow =
        "function Light.is(v) return v end\nlocal v = Vec2({ x = 1 })\nlocal s = Pair({ a = 1 })\n";
    assert_eq!(shadow_home(shadow, 2, "v"), Some((1, 6)));
    assert_eq!(shadow_home(shadow, 0, "s"), Some((2, 6)));
    assert_eq!(shadow_home(shadow, 2, "zz"), None);
}

/// `local x = later()` above `function later()`: the emit writes the
/// global further down, so the checker has no type at the call and
/// prints an error type. The declaration answers.
#[test]
pub(crate) fn a_call_above_its_declaration_hovers_as_the_declaration() {
    let src = "local x = later()\n\nfunction later(): number\n    return 42\nend\n\nprint(x)\n";
    assert_eq!(
        hover_of(src, 0, 10, "type later = *error-type*"),
        "function later(): number"
    );

    // A word no declaration covers keeps the print, and the null guard
    // in the dispatch drops it.
    assert_eq!(
        hover_of(src, 0, 10, "type missing = unknown"),
        "type missing = unknown"
    );
}

/// `self` inside an `impl` read three ways in one file: a read-only view
/// of the fields, a solver variable, and no type at all. The `impl` head
/// names the type once.
#[test]
pub(crate) fn self_reads_as_the_impl_target_at_every_site() {
    let src = concat!(
        "export struct Point as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "export impl Point as\n",
        "    function length(self): number\n",
        "        return self.x\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let at = |printed: &str| name_self_receiver(&format!("```alloy\n{printed}\n```"), doc, 6, 15);
    let named = Some("```alloy\nself: Point\n```".to_string());
    assert_eq!(at("local self: Readonly<Point>"), named);
    assert_eq!(at("self"), named);
    assert_eq!(at("local self: t1"), named);
    assert_eq!(at("local self: {\n    read x: number\n}"), named);

    // The answer the head already writes stays, and a signature that
    // holds `self` is nobody's receiver.
    assert_eq!(at("self: Point"), None);
    assert_eq!(at("function Point.length(self: Point): number"), None);
}

/// A method of an `impl` of an imported struct hovered as `unknown`: the
/// emit writes it on the table the module exports, and that table's type
/// comes from the module, so the method is not in it.
#[test]
pub(crate) fn a_method_of_an_imported_struct_hovers_as_the_source_wrote_it() {
    let src = concat!(
        "import { Point } from \"./point\"\n",
        "\n",
        "export impl Point as\n",
        "    function length(self): number\n",
        "        return self.x\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let start = src.find("length").expect("the name");
    assert_eq!(
        foreign_method_hover(doc, start, start + "length".len()).as_deref(),
        Some("```alloy\nfunction Point.length(self: Point): number\n```")
    );

    // A struct this file declares reads through the child, which types
    // its methods from the class table it builds here.
    let own = concat!(
        "export struct Point as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "impl Point as\n",
        "    function length(self): number\n",
        "        return self.x\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(own);
    let doc = st.docs.get(uri).expect("doc");
    let start = own.find("length").expect("the name");
    assert_eq!(
        foreign_method_hover(doc, start, start + "length".len()),
        None
    );
}

/// `import { Size as VSize } from "./valmod"`: the entry belongs to
/// `valmod`, and another open file's `export type Size` says nothing
/// about it.
#[test]
pub(crate) fn an_import_entry_reads_the_module_the_spec_names() {
    let st = super::support::files(&[
        ("file:///m.aly", "export type Size = number\n"),
        (
            "file:///valmod.aly",
            "export struct Size as\n    n: number\nend\n",
        ),
        (
            "file:///x.aly",
            "import { Size as TSize } from \"./m\"\nimport { Size as VSize } from \"./valmod\"\n",
        ),
    ]);
    let source = &st.docs["file:///x.aly"].source;
    let second = source.rfind("Size as VSize").unwrap();
    let decls = st
        .import_line_decls("file:///x.aly", source, second)
        .unwrap();
    assert!(
        decls
            .iter()
            .any(|d| d.name == "Size" && d.hover.contains("struct")),
        "{decls:?}"
    );

    let first = source.find("Size as TSize").unwrap();
    let decls = st
        .import_line_decls("file:///x.aly", source, first)
        .unwrap();
    assert!(
        decls
            .iter()
            .any(|d| d.name == "Size" && d.hover.contains("type Size")),
        "{decls:?}"
    );
    assert!(
        st.import_line_decls("file:///x.aly", source, source.len() - 1)
            .is_none()
    );
}

/// `print(limit)` above `const limit = 100`: the checker has no type for
/// the name there and prints an error type, so the hover was dropped.
#[test]
pub(crate) fn a_const_used_above_its_line_hovers_as_the_const() {
    let src = "local function show()\n    print(limit)\n end\n\nconst limit = 100\n\nshow()\n";
    assert_eq!(
        hover_of(src, 1, 10, "type limit = *error-type*"),
        "const limit: number"
    );

    // An annotation names the type outright, and a value no literal
    // names reads as the line the author wrote.
    let annotated = "print(scale)\n\nexport const scale: Vector3 = build()\n";
    assert_eq!(
        hover_of(annotated, 0, 6, "type scale = *error-type*"),
        "export const scale: Vector3"
    );

    let called = "print(seed)\n\nconst seed = os.time()\n";
    assert_eq!(
        hover_of(called, 0, 6, "type seed = *error-type*"),
        "const seed = os.time()"
    );

    // A plain `local` below its use is a different name: the global.
    let local = "print(count)\n\nlocal count = 1\n";
    assert_eq!(
        hover_of(local, 0, 6, "type count = *error-type*"),
        "type count = *error-type*"
    );
}
/// A `default` arm of a `match` nested inside a `case` arm: the outer
/// arm still binds the name, and the inner arm is no sibling of it. The
/// walk crosses the inner `match` and the block that closed above.
#[test]
pub(crate) fn a_name_in_a_nested_default_arm_reads_the_outer_binding() {
    const SRC: &str = "enum Shape as\n    Circle(number)\nend\n\nlocal function d(s: Shape, n: number): string\n    return match s with\n        case Circle(x) then\n            match n with\n                case 1 then \"one\"\n                default\n                    `circle: {x}`\n            end\n        default\n            \"other\"\n    end\nend\nprint(d)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let known = st.known_shapes_at(Some(uri));
    let inner = SRC.find("circle: {x}").expect("arm") + "circle: {".len();
    let bound = SRC.find("case Circle(x)").expect("case") + "case Circle(".len();
    let line_of = |o: usize| position_of(SRC, o).0 as usize;

    assert_eq!(
        case_binding_text(doc, line_of(inner), inner, "x", &known),
        Some("```alloy\nx: number\n```\nA binding of `Shape.Circle`.".to_string())
    );
    assert_eq!(
        case_binding_span(doc, line_of(inner), "x", &known),
        Some((bound, bound + 1))
    );
    // The outer `default` is a sibling of the arm, so it binds nothing.
    let other = SRC.find("\"other\"").expect("default");
    assert_eq!(case_binding_span(doc, line_of(other), "x", &known), None);
}

/*
A binding the desugar rewrites, and a hint that says nothing.

`local { x, y } = t` lowers to `local _1 = t local x, y = _1.x, _1.y`.
The names the braces hold are the author's own, and the lowering writes
each one again; the byte after each one is generated, so the map sends
the hint to the head of the line. The temp `_1` is nobody's name.

The braces take no annotation, so a moved hint keeps no edit.

`unknown` and the checker's own `~nil` annotate nothing and tell the
reader nothing, so neither belongs in the gutter.
*/
#[test]
fn a_destructuring_binding_gets_its_type_hints() {
    let src = "local t = { x = 1, y = \"two\" }\nlocal { x, y } = t\nprint(x, y)\n";
    let (st, uri) = super::support::one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let shadow = doc.shadow.lines().nth(1).expect("the lowered line");
    // Right after the name, where a type hint reads.
    let after = |pat: &str, name: usize| (shadow.find(pat).expect(pat) + name) as u32;
    let hint = |character: u32, label: &str| json!({ "kind": 1, "label": label, "position": { "line": 1, "character": character } });

    // `x` sits at column 8 of the source line and `y` at column 11.
    assert_eq!(
        name_end(doc, &hint(after("x,", 1), ": number")),
        Some((1, 9))
    );
    assert_eq!(
        name_end(doc, &hint(after("y =", 1), ": string")),
        Some((1, 12))
    );
    assert_eq!(name_end(doc, &hint(after("_1", 2), ": { }")), None);

    // After the mapping every hint of the line sits on the head of it.
    // The position each one carries says where it belongs.
    let moved = |label: &str, character: u32| {
        json!({
            "kind": 1,
            "label": label,
            "position": { "line": 1, "character": 0 },
            "textEdits": [{
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 0 },
                },
                "newText": label,
            }],
            NAME_END: { "line": 1, "character": character },
        })
    };
    let mut hints = vec![
        moved(": number", 9),
        moved(": string", 12),
        // `for k, v in pairs(counts)` over an untyped record.
        json!({ "kind": 1, "label": ": ~nil", "position": { "line": 2, "character": 7 } }),
        json!({ "kind": 1, "label": ": unknown", "position": { "line": 2, "character": 10 } }),
    ];
    clean_hints(&mut hints, doc);

    let places: Vec<(String, u32)> = hints
        .iter()
        .map(|h| {
            let (_, character) = position_of_value(&h["position"]).expect("position");

            (hint_label(h), character)
        })
        .collect();

    assert_eq!(
        places,
        [(": number".to_string(), 9), (": string".to_string(), 12)],
        "{hints:?}"
    );
    assert!(!hints.iter().any(|h| h.get(NAME_END).is_some()));
    // `local { x: number, y } = t` is not Alloy, so no hint inserts one.
    assert!(
        !hints.iter().any(|h| h.get("textEdits").is_some()),
        "{hints:?}"
    );
}

/*
The alias of `match e as n with`, and the binding of `if local n = e`.

Both emit a `local` of their own, whose name is the author's. The hint
the child sends for it stands after that name, and the byte it sits on
is generated, so the map would send it to the `match` or the `if`.

`match e as n with` takes no annotation on the alias; `if local n = e`
takes one on the binding, so that hint keeps its edit.
*/
#[test]
fn a_rewritten_binding_hints_on_the_name_the_author_wrote() {
    let src = "local function f(s: string): string\n  local test = match s as t with\n    case \"test\" then `{t}test`\n    default \"test\"\n  end\n  return test\nend\nlocal function g(a: string?): string\n  if local p = a then\n    return p\n  end\n  return \"none\"\nend\nprint(f, g)\n";
    let (st, uri) = super::support::one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let at = |line: usize, pat: &str| {
        let text = doc.shadow.lines().nth(line).expect("the lowered line");

        (text.find(pat).expect(pat) + pat.len()) as u32
    };
    let hint = |line: u32, character: u32| {
        json!({
            "kind": 1,
            "label": ": string",
            "position": { "line": line, "character": character },
            "textEdits": [{
                "range": {
                    "start": { "line": line, "character": character },
                    "end": { "line": line, "character": character },
                },
                "newText": ": string",
            }],
        })
    };
    // `t` stands at column 26 of the `match` line and `p` at column 11
    // of the `if` line.
    let alias = hint(1, at(1, ") local t"));
    let binding = hint(8, at(8, "then local p"));

    assert_eq!(name_end(doc, &alias), Some((1, 27)));
    assert_eq!(name_end(doc, &binding), Some((8, 12)));

    let mut hints = vec![alias, binding];

    for h in hints.iter_mut() {
        let (line, character) = name_end(doc, h).expect("the name");
        h[NAME_END] = json!({ "line": line, "character": character });
        // The map sends the hint to the head of the statement.
        h["position"] = json!({ "line": line, "character": 2 });
    }

    clean_hints(&mut hints, doc);

    assert_eq!(
        position_of_value(&hints[0]["position"]),
        Some((1, 27)),
        "{:?}",
        hints[0]
    );
    assert_eq!(
        position_of_value(&hints[1]["position"]),
        Some((8, 12)),
        "{:?}",
        hints[1]
    );
    // `match s as t: string with` is not Alloy; `if local p: string = a`
    // is.
    assert!(hints[0].get("textEdits").is_none(), "{:?}", hints[0]);
    assert_eq!(
        hints[1].pointer("/textEdits/0/range/start"),
        Some(&json!({ "line": 8, "character": 12 })),
        "{:?}",
        hints[1]
    );
}

/// `new Pair<<number, string>>` writes the arguments the print drops:
/// Luau names a struct by its metatable, which carries none.
#[test]
fn a_generic_struct_keeps_the_arguments_the_new_wrote() {
    const SRC: &str = "struct Pair<A, B> as\n    first: A,\n    second: B,\nend\n\nlocal p = new Pair<<number, string>> { first = 1, second = \"one\" }\nprint(p)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let line = position_of(SRC, SRC.find("local p").expect("the binding")).0;

    assert_eq!(
        crate::proxy::hover::source_type(doc, line, 6),
        Some("Pair<number, string>".to_string())
    );
    assert_eq!(
        prefer_constructed_struct("```alloy\nlocal p: Pair\n```", doc, line, 6),
        Some("```alloy\nlocal p: Pair<number, string>\n```".to_string())
    );

    // The hint on the same binding reads the same way, and its edit
    // inserts a type the file compiles.
    let mut hints = vec![json!({
        "kind": 1,
        "label": ": Pair",
        "position": { "line": line, "character": 7 },
    })];
    clean_hints(&mut hints, doc);

    assert_eq!(hint_label(&hints[0]), ": Pair<number, string>");
    assert_eq!(
        hints[0]["textEdits"][0]["newText"],
        json!(": Pair<number, string>")
    );
}

/// A use of the binding below the `new` reads the same arguments: the
/// print names the metatable, and the metatable carries none.
#[test]
fn a_generic_struct_keeps_its_arguments_where_the_binding_is_used() {
    const SRC: &str =
        "struct Box<T> as\n    v: T\nend\n\nlocal b = new Box<<number>> { v = 42 }\nprint(b)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let line = position_of(SRC, SRC.find("print(b)").expect("the use")).0;

    assert_eq!(
        prefer_constructed_struct("```alloy\nlocal b: Box\n```", doc, line, 6),
        Some("```alloy\nlocal b: Box<number>\n```".to_string())
    );
}

/// A function after a closed `trait` block is no member of the trait:
/// the block's own `end` closes the head the scan found.
#[test]
pub(crate) fn a_closed_block_gives_self_no_type_below_it() {
    let src = "trait Ord as\n    function compare(self, other: Ord): number\nend\n\nfunction largest<T: Ord>(xs: { T }): T\n    return xs[1]\nend\n";
    let (st, uri) = super::support::one_file(src);
    let doc = &st.docs[uri];
    assert_eq!(impl_self_type(doc, 1), Some("Ord".to_string()));
    assert_eq!(impl_self_type(doc, 5), None);
}

/// Two structs of one field set print alike, so the child may name
/// either. The constructor path on the line says which, and a member of
/// a namespace names itself through its path.
#[test]
fn a_namespace_constructor_names_its_own_struct() {
    const SRC: &str = "struct Point as\n    x: number\n    y: number\nend\n\nnamespace Geo as\n    struct Vec2 as\n        x: number\n        y: number\n    end\n\n    impl Vec2 as\n        function new(x: number): Vec2\n            return new Vec2 { x = x, y = 0 }\n        end\n    end\nend\n\nlocal e = Geo.Vec2.new(5)\nlocal p = Point.new(1, 2)\nprint(e, p)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let line_of = |needle: &str| position_of(SRC, SRC.find(needle).expect(needle)).0;
    let vec2 = line_of("local e =");
    let point = line_of("local p =");

    assert_eq!(
        crate::proxy::hover::source_type(doc, vec2, 6),
        Some("Geo.Vec2".to_string())
    );
    // The print names `Point`, the struct of the same shape the child
    // met first; the path on the line names the one the reader wrote.
    assert_eq!(
        prefer_constructed_struct("```alloy\nlocal e: Point\n```", doc, vec2, 6),
        Some("```alloy\nlocal e: Geo.Vec2\n```".to_string())
    );
    // A top level constructor still names its own struct.
    assert_eq!(
        crate::proxy::hover::source_type(doc, point, 6),
        Some("Point".to_string())
    );
}

/// A doc comment stands on the declaration of a member, and every
/// hover of that member reads it: a field where it is used, and a
/// method through the trait that requires it.
#[test]
fn a_member_carries_its_doc_comment_to_every_hover() {
    const SRC: &str = "export struct Player as\n    --- The player's health points.\n    hp: number\nend\n\nexport trait Greeter as\n    --- Says hello.\n    function greet(self, name: string): string\nend\n\nexport struct Bot as\n    id: number\nend\n\nimpl Greeter for Bot as\n    function greet(self, name: string): string\n        return \"hi\"\n    end\nend\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");

    assert_eq!(
        member_doc(doc, "Player", "hp"),
        Some("The player's health points.".to_string())
    );
    // The `impl` writes no comment of its own, so the trait's stands.
    assert_eq!(member_doc(doc, "Bot", "greet"), None);
    assert_eq!(
        name_method_doc(
            "```alloy\nfunction Bot:greet(self: Bot, name: string): string\n```",
            doc
        ),
        Some(
            "```alloy\nfunction Bot:greet(self: Bot, name: string): string\n```\n\nSays hello."
                .to_string()
        )
    );
    // A hover that already carries text keeps it.
    assert_eq!(
        name_method_doc("```alloy\nfunction Bot:greet(self: Bot)\n```\n\nSaid.", doc),
        None
    );
}

/// An attribute's own parameter hovers at its declaration. The emit
/// writes the parameter list, so the child has no word for the name.
#[test]
pub(crate) fn an_attribute_parameter_hovers_at_its_declaration() {
    let src = "attribute icon(asset: string) on struct\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let start = src.find("asset").expect("asset");
    let answer = declared_parameter_hover(doc, start, start + "asset".len()).expect("hover");

    assert_eq!(
        answer,
        "```alloy\nasset: string\n```\nA parameter of `attribute icon`."
    );
}

/// A hint on a `try` of a generic enum: the child cuts each arm of the
/// Result, `{ read _1: T, ... 4 more ... }`, so no pair fold finds a
/// `tag`. The method table's arguments name the Result, and no payload
/// slot reaches the gutter.
#[test]
fn a_cut_result_hint_reads_by_the_method_tables_arguments() {
    let module = "export enum Opt<T> as\n    Some(T),\n    Nil\nend\n";
    let user = "import { Opt } from \"./opt\"\n\nfunction findFirst(xs: {number}): Opt<number>\n    return Opt.Nil\nend\n\nlocal r = try do\n    return findFirst({1, 2, 30})\nend\n";
    let dir = std::env::temp_dir().join(format!("alloy-hint-cut-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");

    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        snippets: true,
        ..State::default()
    };
    let mut uris = Vec::new();

    for (rel, src) in [("opt.aly", module), ("tryenum.aly", user)] {
        let path = dir.join("src").join(rel);
        std::fs::write(&path, src).expect(rel);
        let uri = format!("file://{}", path.display());
        let options = EmitOptions {
            file_name: path.to_string_lossy().into_owned(),
            ..EmitOptions::default()
        };
        st.docs.insert(
            uri.clone(),
            Doc::new(
                src.to_string(),
                1,
                &options,
                &alloy::luaux::Config::default(),
                None,
            ),
        );
        uris.push(uri);
    }

    let uri = uris[1].as_str();
    let doc = st.docs.get(uri).expect("doc");
    let line = position_of(user, user.find("local r").expect("the binding")).0;
    let raw = ": (ResultMethods<\"Nil\" | { @metatable t1, { _1: number, tag: \"Some\" } }, any> & { read _1: \"Nil\" | { @metatable t1, { _1: number, tag: \"Some\" } }, ... 4 more ... }) | (ResultMethods<\"Nil\" | { @metatable t1, { _1: number, tag: \"Some\" } }, any> & { read _1: any, read __err: any, read __ok: \"Nil\" | { @metatable t1, { _1: number, tag: \"Some\" } }, ... 2 more ... }) where t1 = { Nil: \"Nil\" | { @metatable t1, { _1: any, tag: \"Some\" } }, ... 4 more ... }";
    let mut hints = json!([{
        "kind": 1,
        "label": raw,
        "position": { "line": line, "character": 7 },
        "textEdits": [],
    }]);
    alloy::shapes::fold_value(&mut hints, &st.known_shapes_at(Some(uri)));
    let hints = hints.as_array_mut().expect("hints");
    clean_hints(hints, doc);
    let _ = std::fs::remove_dir_all(&dir);
    let label = hint_label(hints.first().expect("the hint stays"));

    assert_eq!(label, ": Result<Opt<number>, any>");

    for slot in ["_1", "__err", "__ok", " more ..."] {
        assert!(!label.contains(slot), "{label}");
    }
}

/// A function called above its declaration gets a forward `local`, and
/// its declaration assigns that local. The child prints the local's
/// optional type on the declaration's own name; the header the caret
/// sits on says what the function is, the way a call site reads it.
#[test]
fn a_hoisted_function_s_declaration_reads_its_own_header() {
    let src = "function isEven(n: number): boolean\n    return isOdd(n - 1)\nend\n\nfunction isOdd(n: number): boolean\n    return isEven(n - 1)\nend\n";
    let (st, uri) = super::support::one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "```luau\nfunction isOdd: ((n: number) -> boolean)?\n```";
    assert_eq!(
        declared_signature(printed, doc, 4, 9).as_deref(),
        Some("```luau\nfunction isOdd(n: number): boolean\n```")
    );

    // The same print on a line that declares nothing is the child's.
    assert_eq!(declared_signature(printed, doc, 1, 11), None);
}

/// `$assert_eq(p:len(), 5)` expands to generated text that quotes the
/// argument for its message and writes it again as code. The hover
/// home is the code, not the quoted copy, for an intrinsic and for a
/// macro of the file's own.
#[test]
pub(crate) fn a_hover_in_a_macro_argument_lands_on_the_code() {
    let head = concat!(
        "struct Point as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "impl Point as\n",
        "    function len(self): number\n",
        "        return self.x\n",
        "    end\n",
        "end\n",
        "\n",
        "macro m(v)\n",
        "    print(\"v\", v)\n",
        "end\n",
        "\n",
        "local p = new Point { x = 3 }\n",
    );

    for call in ["$assert_eq(p:len(), 3)", "$dbg(p:len())", "$m(p:len())"] {
        let src = format!("{head}{call}\n");
        let (st, uri) = one_file(&src);
        let doc = st.docs.get(uri).unwrap();
        let (line, column) =
            shadow_home(&doc.shadow, 15, "len").unwrap_or_else(|| panic!("{call}"));
        let text = doc.shadow.lines().nth(line as usize).unwrap();
        let at = offset_of(text, 0, column).unwrap();

        assert!(text[at..].starts_with("len()"), "{call}: {text}");
        assert!(text[..at].ends_with("p:"), "{call}: {text}");
        assert!(!context::in_string(text, at), "{call}: {text}");
    }
}

/// A `const` bound to a table literal hovers with the record the child
/// printed at its declaration. The fold names every other print of
/// that shape `typeof(SCHEMA)`, and the restyle has already written
/// the source's keywords over the child's `local`.
#[test]
fn a_const_table_hovers_as_its_record_at_the_declaration() {
    let src = "export const SCHEMA = {\n  stats = {\n    strength = { default = 0, kind = \"int\" },\n  },\n}\n\nprint(SCHEMA.stats)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "```luau\nlocal SCHEMA: {\n    stats: {\n        strength: {\n            default: number,\n            kind: \"int\"\n        }\n    }\n}\n```";
    let text = restyle_hover(printed, doc, 0, 13).expect("restyled");
    let text = alloy::shapes::fold(&text, &st.known_shapes_at(Some(uri)));

    assert!(
        text.starts_with("```alloy\nexport const SCHEMA: {"),
        "{text}"
    );
    assert!(text.contains("default: number"), "{text}");
    assert!(!text.contains("typeof("), "{text}");
}

/// A key of a table literal a `const` binds hovers as the entry the
/// child printed for it in the record of the binding: nested keys, and
/// a key whose value is a plain literal. A key inside a call's
/// argument belongs to no entry of the binding.
#[test]
fn a_key_of_a_const_table_hovers_as_its_entry() {
    let src = "export const SCHEMA = {\n  stats = {\n    strength = Scribe.Int(0, { Min = 0 }),\n  },\n  x = 1,\n}\n";
    let printed = "```alloy\nexport const SCHEMA: {\n    stats: {\n        strength: {\n            default: number,\n            kind: \"int\"\n        }\n    },\n    x: number\n}\n```";
    let path = |line: u32, character: u32| {
        let Caret { offset, .. } = Caret::at(src, line, character).expect("word");

        literal_key(src, offset)
    };
    let name = src.find("SCHEMA").expect("name");
    let strength = path(2, 4).expect("strength");
    let stats = path(1, 2).expect("stats");
    let x = path(4, 2).expect("x");

    assert_eq!(
        strength,
        (name, vec!["stats".to_string(), "strength".to_string()])
    );
    assert_eq!(stats.1, vec!["stats".to_string()]);
    assert_eq!(x.1, vec!["x".to_string()]);
    assert_eq!(path(2, 31), None);
    assert_eq!(path(0, 13), None);
    assert_eq!(
        record_entry(printed, &strength.1).as_deref(),
        Some("```alloy\nstrength: {\n    default: number,\n    kind: \"int\"\n}\n```")
    );
    assert_eq!(
        record_entry(printed, &stats.1).as_deref(),
        Some(
            "```alloy\nstats: {\n    strength: {\n        default: number,\n        kind: \"int\"\n    }\n}\n```"
        )
    );
    assert_eq!(
        record_entry(printed, &x.1).as_deref(),
        Some("```alloy\nx: number\n```")
    );
    assert_eq!(record_entry(printed, &["y".to_string()]), None);
}

/// `Hit.Swing` reads the variant, not the remote `Swing` the file
/// imports: the member is the enum's. The bare name and a path through
/// a star import still read the remote's declaration.
#[test]
pub(crate) fn a_variant_of_an_imported_name_is_no_import_hover() {
    let src = "import { Swing } from \"./net\"\nimport * as Net from \"./net\"\nenum Hit as\n    Swing(number)\n    Miss\nend\nprint(Hit.Swing(1), Swing, Net.Swing)\n";
    let mut state = super::support::files(&[("file:///f.aly", src)]);
    state
        .docs
        .get_mut("file:///f.aly")
        .expect("doc")
        .import_sources = vec!["export remote Swing(n: number) from client\n".to_string()];
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(std::io::sink()),
        Vec::new(),
        None,
    );
    *server.state.lock().expect("state") = state;
    let at = |needle: &str| {
        let (line, character) = position_of(src, src.find(needle).expect("the name"));

        json!({ "params": {
            "textDocument": { "uri": "file:///f.aly" },
            "position": { "line": line, "character": character },
        } })
    };
    let hover = |needle: &str| server.source_binding_hover("file:///f.aly", &at(needle), &json!(1));

    assert!(!hover("Swing(1)"));
    assert!(hover("Swing, Net"));
    assert!(hover("Swing)\n"));
}

/// A method's name belongs to the file that declares it. Another file
/// may declare a struct of the same spelling, and that declaration is
/// no answer for the method: the child types the method itself.
#[test]
pub(crate) fn another_file_s_struct_is_no_method_hover() {
    let state = super::support::files(&[
        (
            "file:///a.aly",
            "struct Provider as\nend\n\nimpl Provider as\n    public function Test()\n    end\nend\n",
        ),
        ("file:///b.aly", "export struct Test as\nend\n"),
    ]);
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(std::io::sink()),
        Vec::new(),
        None,
    );
    *server.state.lock().expect("state") = state;

    let at = |line: u32, character: u32| {
        json!({ "params": {
            "textDocument": { "uri": "file:///a.aly" },
            "position": { "line": line, "character": character },
        } })
    };

    assert!(!server.declaration_hover("file:///a.aly", &at(4, 21), &json!(1)));
    // The struct's own name still reads its declaration.
    assert!(server.declaration_hover("file:///a.aly", &at(0, 8), &json!(1)));
}

/// The checker prints a type that holds an imported struct as a solver
/// variable with a `where` clause. The local reads what its first value
/// names: the collection its `new` builds, or the field it reads.
#[test]
fn a_solver_variable_local_reads_its_first_value() {
    const SRC: &str = "struct Inventory as\n    slots: HashMap<number, Item>\nend\n\nstruct Save as\n    inventory: Inventory\nend\n\nlocal b = new HashMap<<number, Item>>()\nlocal save: Save? = nil\nlocal slots = save?.inventory.slots\nprint(b, slots)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let named = |printed: &str, line: u32, character: u32| {
        crate::proxy::hover::name_solver_local(
            &st,
            &format!("```luau\n{printed}\n```"),
            doc,
            line,
            character,
        )
    };

    assert_eq!(
        named(
            "local b: t2 where t1 = {\n    [number]: t6\n} ; t2 = {}",
            8,
            6
        )
        .as_deref(),
        Some("```luau\nlocal b: HashMap<number, Item>\n```")
    );
    // A use below reads the same `new`.
    assert_eq!(
        named("local b: t7", 11, 6).as_deref(),
        Some("```luau\nlocal b: HashMap<number, Item>\n```")
    );
    assert_eq!(
        named("local slots: HashMap<number, t1>? where t1 = {}", 10, 7).as_deref(),
        Some("```luau\nlocal slots: HashMap<number, Item>?\n```")
    );
    // A print with no solver variable stays.
    assert_eq!(named("local b: HashMap<number, string>", 8, 6), None);
}

/// A callback's parameter belongs to the lambda around it, not to an
/// earlier function that takes a parameter by the same name.
#[test]
fn a_callback_parameter_names_the_lambda_it_belongs_to() {
    let src = "local function load(player: Player)\n    print(player)\nend\n\nPlayers.PlayerAdded:Connect(function(player)\n    print(player.Name)\nend)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let hover = |line: u32, character: u32| {
        unlocal_parameter("```luau\nlocal player: Player\n```", doc, line, character)
    };

    assert_eq!(
        hover(5, 11).as_deref(),
        Some("```luau\nplayer: Player\n```\nA parameter of an anonymous function.")
    );
    assert_eq!(
        hover(1, 11).as_deref(),
        Some("```luau\nplayer: Player\n```\nA parameter of `function load`.")
    );
}

/// A nested argument keeps its own `>`: the list of `<<...>>` closes at
/// bracket depth zero. The hint once read `HashMap<string, Array<number>`.
#[test]
fn a_nested_type_argument_keeps_its_bracket() {
    const SRC: &str = "local a = new HashMap<<string, Array<number>>>()\nlocal b = HashMap.new<<string, (number) -> number>>()\nlocal c = new Set<<Array<number>>>()\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");

    assert_eq!(
        source_type(doc, 0, 6).as_deref(),
        Some("HashMap<string, Array<number>>")
    );
    assert_eq!(
        source_type(doc, 1, 6).as_deref(),
        Some("HashMap<string, (number) -> number>")
    );
    assert_eq!(
        source_type(doc, 2, 6).as_deref(),
        Some("Set<Array<number>>")
    );
}

/// An expression match lowers to one expression, so the child types a
/// bare binding by the arm's result: `k` read as `string` for a match on
/// a number. The binding reads the value the match takes, and `...rest`
/// reads an array of the element.
#[test]
fn an_expression_match_binding_reads_the_matched_value() {
    const SRC: &str = "local n = 7\nlocal r = match n with\n    case k where k > 5 then \"big\"\n    default \"small\"\nend\nlocal t = { 1, 2 }\nlocal f = match t with\n    case [first, ...rest] then first\n    default 0\nend\nprint(r, f)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let known = st.known_shapes_at(Some(uri));
    let hover = |needle: &str, word: &str| {
        let at = SRC.find(needle).expect("needle");

        case_binding_text(doc, position_of(SRC, at).0 as usize, at, word, &known)
    };

    assert_eq!(
        hover("k where", "k").as_deref(),
        Some("```alloy\nk: number\n```\nA binding of `match n`.")
    );
    assert_eq!(
        hover("rest]", "rest").as_deref(),
        Some("```alloy\nrest: number[]\n```\nA binding of the array pattern.")
    );
}

/// The checker prints a value of a namespace struct by its shape: a
/// solver variable with a `where` clause that holds the field list. The
/// hover names the one struct in reach with those fields, as the hint
/// already does.
#[test]
fn a_namespace_struct_value_hovers_by_its_name() {
    const SRC: &str = "namespace Geo\n  struct Vec2\n    x: number\n  end\n  function make(): Vec2\n    return new Vec2 { x = 1 }\n  end\nend\nlocal p = Geo.make()\nprint(p)\n";
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "t2 where t1 = {\n    new: (f: {\n        x: number\n    }) -> t2\n} ; t2 = { @metatable t1,\n{\n    x: number\n} }";
    let named = |text: String| crate::proxy::hover::name_solver_struct(&text, doc, &[]);

    assert_eq!(
        named(format!("```luau\nlocal p: {printed}\n```")).as_deref(),
        Some("```luau\nlocal p: Geo.Vec2\n```")
    );
    // A field read prints the type alone.
    assert_eq!(
        named(format!("```luau\n{printed}\n```")).as_deref(),
        Some("```luau\nGeo.Vec2\n```")
    );
    assert_eq!(named("```luau\nlocal p: number\n```".to_string()), None);
    // The variable stands inside a larger type.
    assert_eq!(
        named(format!(
            "```luau\nlocal ps: {{t2}}? where {}\n```",
            &printed[9..]
        ))
        .as_deref(),
        Some("```luau\nlocal ps: {Geo.Vec2}?\n```")
    );
}

/// A remote of `net` sends `{ Stack }`, and only `net` imports `Stack`.
/// The client reaches the struct through `net`: the hover names it, and
/// `s.` offers no `new`. An enum field of a struct binds its own
/// variable ahead of the struct's in the clause.
#[test]
fn a_struct_a_remote_sends_is_named_through_its_module() {
    let dir = std::env::temp_dir().join(format!("alloy-remote-struct-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .expect("alloy.toml");
    let sources = [
        (
            "items",
            "export enum Kind\n    A\n    B(number)\nend\n\nexport struct Stack\n    count: number\nend\n\nexport struct Slot\n    count: number\n    kind: Kind\nend\n",
        ),
        (
            "net",
            "import { Stack, Slot } from \"./items\"\n\nexport remote Stacks(stacks: { Stack }, slot: Slot) from server\n",
        ),
        (
            "client",
            "import { Stacks } from \"./net\"\n\nStacks.on(function(stacks, slot)\n    for _, s in stacks do\n        print(s.count, slot)\n    end\nend)\n",
        ),
    ];
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        ..State::default()
    };

    for (name, source) in sources {
        let path = dir.join(format!("src/{name}.aly"));
        std::fs::write(&path, source).expect("source");
        let uri = path_to_uri(&path);
        let (options, jsx) = st.options_for(&uri);
        st.docs
            .insert(uri, Doc::new(source.to_string(), 1, &options, &jsx, None));
    }

    let client = path_to_uri(&dir.join("src/client.aly"));
    let doc = st.docs.get(&client).expect("doc");
    let imported = st.imported_docs(&client);
    let decls: Vec<_> = imported
        .iter()
        .flat_map(|d| d.import_decls.iter())
        .collect();
    let shapes: Vec<_> = imported
        .iter()
        .flat_map(|d| d.import_shapes.iter())
        .collect();
    let stack = "```luau\nlocal s: t1 where t1 = { @metatable t2,\n{\n    count: number\n} } ; t2 = {\n    __index: t2,\n    new: (f: {\n        count: number\n    }) -> t1\n}\n```";
    let slot = "```luau\nlocal slot: t3 where t1 = \"A\" | { @metatable t2,\n{\n    _1: number,\n    tag: \"B\"\n} } ; t2 = {\n    __index: t2\n} ; t3 = { @metatable t4,\n{\n    count: number,\n    kind: t1\n} } ; t4 = {\n    __index: t4\n}\n```";

    assert_eq!(name_solver_struct(stack, doc, &[]), None);
    assert_eq!(
        name_solver_struct(stack, doc, &decls).as_deref(),
        Some("```luau\nlocal s: Stack\n```")
    );
    assert_eq!(
        name_solver_struct(slot, doc, &decls).as_deref(),
        Some("```luau\nlocal slot: Slot\n```")
    );

    let mut result = json!([
        { "label": "new", "kind": 3, "detail": "({ count: number }) -> Stack" },
        { "label": "count", "kind": 5, "detail": "number" },
    ]);
    clean_completion(&mut result, doc, &shapes, 4, 16, true);
    assert_eq!(result.as_array().map(Vec::len), Some(1), "{result}");
    assert_eq!(result[0]["label"], "count");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file-level local and a parameter may share a name. Inside the
/// function the parameter is the one in scope, so its hover names the
/// function and not the local.
#[test]
fn a_parameter_shadows_a_file_local_of_its_name() {
    let src = "local count = \"top\"\nlocal function bump(count: number): number\n    return count + 1\nend\nprint(bump(1), count)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");

    assert_eq!(
        unlocal_parameter("```luau\nlocal count: number\n```", doc, 2, 12).as_deref(),
        Some("```luau\ncount: number\n```\nA parameter of `function bump`.")
    );
    // The local itself keeps its own hover.
    assert_eq!(
        unlocal_parameter("```luau\nlocal count: string\n```", doc, 4, 16),
        None
    );
}

/// `Light.Active` under `import { Status as Light }`: the hover reads
/// the variant of the enum the alias names, and another module's
/// `Status` stays out of it.
#[test]
fn an_aliased_variant_hovers_as_its_enum_s() {
    use super::documents::Recorder;

    let state = super::support::files(&[
        (
            "file:///a.aly",
            "export enum Status as\n    Active\n    Closed(number)\nend\n",
        ),
        (
            "file:///b.aly",
            "export enum Status as\n    Active\n    Off\nend\n",
        ),
        (
            "file:///f.aly",
            "import { Status } from \"./a\"\nimport { Status as Light } from \"./b\"\nprint(Status.Closed(1), Light.Active)\n",
        ),
    ]);
    let log = Arc::new(Mutex::new(Vec::new()));
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(Recorder(Arc::clone(&log))),
        Vec::new(),
        None,
    );
    *server.state.lock().expect("state") = state;
    let src = "import { Status } from \"./a\"\nimport { Status as Light } from \"./b\"\nprint(Status.Closed(1), Light.Active)\n";
    let (line, character) = position_of(src, src.find("Active)").unwrap());
    let message = json!({ "params": {
        "textDocument": { "uri": "file:///f.aly" },
        "position": { "line": line, "character": character },
    } });

    assert!(server.declaration_hover("file:///f.aly", &message, &json!(1)));

    let sent = String::from_utf8_lossy(&log.lock().expect("the log")).into_owned();
    assert!(sent.contains("Status.Active"), "{sent}");
}

/// A child lookup hovers with the type the check artifact gives it: the
/// compiler's cast, or Luau's own signature for a plain call. A
/// `wait_timeout` makes `=>` optional, and a guarded link inside a chain
/// is a plain `FindFirstChild`.
#[test]
fn a_child_lookup_hovers_with_the_compiler_s_cast() {
    let src = "const a = workspace=>Baseplate\nconst b = workspace->Baseplate\nconst c = workspace->Model->Part\nprint(a, b, c)\n";
    let doc = |wait_timeout| {
        let options = EmitOptions {
            wait_timeout,
            ..EmitOptions::default()
        };

        Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        )
    };
    let cast = |d: &Doc, needle: &str| child_cast(d, src.find(needle).expect("the lookup") + 2);
    let timed = doc(Some(5.0));

    assert_eq!(cast(&timed, "=>Baseplate").as_deref(), Some("Instance?"));
    assert_eq!(cast(&timed, "->Baseplate").as_deref(), Some("Instance?"));
    assert_eq!(cast(&timed, "->Model").as_deref(), Some("Instance?"));
    assert_eq!(cast(&timed, "->Part").as_deref(), Some("Instance?"));
    assert_eq!(cast(&doc(None), "=>Baseplate").as_deref(), Some("Instance"));

    let hover = keywords::child_hover(src, src.find("=>Baseplate").unwrap() + 2, |at| {
        child_cast(&timed, at)
    })
    .expect("a hover");
    assert!(
        hover
            .2
            .starts_with("```alloy\nworkspace=>Baseplate: Instance?\n```"),
        "{}",
        hover.2
    );
}

/// A child name hovers with the type luau-lsp gives the name that holds
/// the lookup: a temp of the chain, or a binding whose whole value it
/// is. With no such name, the child types the expression at the `)`
/// that closes the lookup: the call, or the group of its cast. A
/// sourcemap then names the class, so the hover drops the note that no
/// source names one.
#[test]
fn a_child_name_asks_the_name_that_holds_its_lookup() {
    let doc = |src: &str, wait_timeout| {
        let options = EmitOptions {
            wait_timeout,
            ..EmitOptions::default()
        };

        Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        )
    };
    let home_with = |src: &str, needle: &str, wait_timeout| {
        let d = doc(src, wait_timeout);
        let at = src.find(needle).expect("the lookup") + 2;

        child_value_home(&d, at).map(|(l, c)| {
            let text = d.shadow.lines().nth(l as usize).expect("the line");

            text.chars().skip(c as usize).take(2).collect::<String>()
        })
    };
    let home = |src: &str, needle: &str| home_with(src, needle, Some(5.0));
    let chain = "const k = ReplicatedStorage=>Assets->Swords->Katana\nprint(k)\n";

    // `_1` holds `=>Assets`, `_2` holds `->Swords`, and `k` the rest.
    assert_eq!(home(chain, "=>Assets").as_deref(), Some("_1"));
    assert_eq!(home(chain, "->Swords").as_deref(), Some("_2"));
    assert_eq!(home(chain, "->Katana").as_deref(), Some("k "));

    // A value that goes on past the lookup, an annotation, and a field
    // after it hold something else. A temp the block assigns again
    // types as the union of its values. Each one asks at the `)`.
    for (src, needle, at) in [
        ("const n = workspace->A == nil\nprint(n)\n", "->A", "))"),
        ("const m: Model = workspace=>A\nprint(m)\n", "=>A", ")"),
        ("const d = workspace=>A.Size\nprint(d)\n", "=>A", ") "),
        (
            "const a = workspace=>A->B\nconst b = workspace=>C->D\nprint(a, b)\n",
            "=>A",
            ") ",
        ),
    ] {
        assert_eq!(home(src, needle).as_deref(), Some(at), "{src}");
    }

    // With no timeout a chain holds no temp: the middle link is the
    // call that the next link calls a method of.
    let untimed = "const b = ReplicatedStorage=>Shared=>net\nprint(b)\n";
    assert_eq!(home_with(untimed, "=>Shared", None).as_deref(), Some("):"));

    let at = chain.find("->Katana").unwrap() + 2;
    let hover = child_lookup_hover(
        "```luau\nlocal k: Tool?\n```",
        &doc(chain, Some(5.0)),
        0,
        at as u32,
    )
    .expect("a hover")
    .0;
    assert!(
        hover.starts_with("```alloy\nReplicatedStorage=>Assets->Swords->Katana: Tool?\n```"),
        "{hover}"
    );
    assert!(!hover.contains("names no class"), "{hover}");

    // At a `)` the child prints the type alone.
    let at = untimed.find("=>Shared").unwrap() + 2;
    let hover = child_lookup_hover("```luau\nFolder\n```", &doc(untimed, None), 0, at as u32)
        .expect("a hover")
        .0;
    assert!(
        hover.starts_with("```alloy\nReplicatedStorage=>Shared: Folder\n```"),
        "{hover}"
    );
}

/// A completion after `->` asks inside the string the lookup lowers to,
/// where the child lists the children. With no name yet the name is the
/// word the parser took from the next line, or the repair's placeholder.
#[test]
fn a_child_completion_lands_in_the_string_of_its_call() {
    for (src, typed, written) in [
        ("const up = script.Parent->sys\n", "->sys", "sys\""),
        ("const up = script.Parent=>\nprint(up)\n", "=>", "print\""),
        ("print(script.Parent->)\n", "->", "__alloy_hole\""),
    ] {
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let caret = src.find(typed).expect("the lookup") + typed.len();
        let start = context::child_name_start(src, caret).expect("a child name");
        let (_, text, name) = child_call(doc, start).unwrap_or_else(|| panic!("{src}"));

        assert_eq!(start, src.find(typed).unwrap() + 2, "{src}");
        assert!(text[name..].starts_with(written), "{src}: {text}");
    }

    // A function type holds no call.
    let (st, uri) = one_file("local f: (number) -> string = tostring\n");
    let doc = st.docs.get(uri).expect("doc");
    assert!(child_call(doc, doc.source.find("string").unwrap()).is_none());
}

/// A hover shows the value the declaration wrote, and only where the
/// binding still holds it: not under a `local` with no value, and not
/// at a use once a later statement assigns it again. A `const` keeps
/// its value.
#[test]
fn an_initializer_shows_only_while_the_binding_holds_it() {
    let src = concat!(
        "struct P as\n",
        "    n: number\n",
        "end\n",
        "\n",
        "local p = new P { n = 1 }\n",
        "p = new P { n = 2 }\n",
        "print(p)\n",
        "local q: P\n",
        "q = new P { n = 3 }\n",
        "print(q)\n",
        "const c = new P { n = 4 }\n",
        "print(c)\n",
        "local k = new P { n = 5 }\n",
        "print(k)\n",
        "local r = new P { n = 6 }\n",
        "local x = 0; r = new P { n = 7 }\n",
        "print(r)\n",
        "local s = new P { n = 8 }\n",
        "do local s = 0; s = 1 end\n",
        "print(s)\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let hover = |name: &str, line: u32| {
        append_initializer(&format!("```alloy\nlocal {name}: P\n```"), doc, line, 6)
    };

    assert!(hover("p", 4).is_some_and(|t| t.contains("n = 1")));
    assert_eq!(hover("p", 6), None);
    assert_eq!(hover("q", 7), None);
    assert_eq!(hover("q", 9), None);
    assert!(hover("c", 11).is_some_and(|t| t.contains("n = 4")));
    assert!(hover("k", 13).is_some_and(|t| t.contains("n = 5")));
    // A write after a `;` counts, and a write to an inner `local` of the
    // same name does not.
    assert_eq!(hover("r", 16), None);
    assert!(hover("s", 19).is_some_and(|t| t.contains("n = 8")));
}

/// Three functions each declare a `bag`. A hover reads the declaration
/// in scope: its keyword, and its value or none.
#[test]
fn a_hover_reads_the_declaration_in_scope_of_a_shared_name() {
    let src = concat!(
        "struct Bag\n",
        "    n: number = 0\n",
        "end\n",
        "local function fresh(): Bag\n",
        "    const bag = new Bag {}\n",
        "    return bag\n",
        "end\n",
        "local function other(): Bag\n",
        "    const bag = fresh()\n",
        "    return bag\n",
        "end\n",
        "local function third(): Bag\n",
        "    local bag = new Bag { n = 5 }\n",
        "    return bag\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let hover = |line: u32, character: u32| {
        let child = "```luau\nlocal bag: Bag\n```";
        let text = restyle_hover(child, doc, line, character).unwrap_or(child.to_string());

        append_initializer(&text, doc, line, character).unwrap_or(text)
    };

    assert_eq!(hover(5, 12), "```alloy\nconst bag: Bag = new Bag {}\n```");
    assert_eq!(hover(8, 11), "```alloy\nconst bag: Bag\n```");
    assert_eq!(hover(9, 12), "```alloy\nconst bag: Bag\n```");
    assert_eq!(
        hover(12, 11),
        "```luau\nlocal bag: Bag = new Bag { n = 5 }\n```"
    );
    assert_eq!(
        hover(13, 12),
        "```luau\nlocal bag: Bag = new Bag { n = 5 }\n```"
    );
}

/// A name a std import binds hovers as the std: a star alias as its
/// module, an alias as the name it renames, and a type with no doc
/// entry as the runtime declares it.
#[test]
fn a_std_import_name_hovers_as_the_std() {
    let src = concat!(
        "import * as serde from \"@alloy/std/serde\"\n",
        "import { HashMap as Map } from \"@alloy/std/collections\"\n",
        "import { SignalConnection } from \"@alloy/std/signal\"\n",
        "local m = new Map<<string, number>>()\n",
        "local c: SignalConnection? = nil\n",
        "print(m, c, x.serde)\n",
    );
    let at = |needle: &str| {
        let start = src.find(needle).expect("the name");
        let word = needle
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap();

        super::super::hover::std_import_hover(src, word, start)
    };

    let module = at("serde from").expect("the module");
    assert!(
        module.starts_with("```alloy\nimport * as serde from \"@alloy/std/serde\"\n```"),
        "{module}"
    );
    assert!(
        module.contains("`Serialize`") && module.contains("`@rename`"),
        "{module}"
    );
    assert!(!module.contains("try_block"), "{module}");

    let alias = at("Map<<").expect("the alias");
    assert!(
        alias.contains("The std `HashMap`, imported as `Map`"),
        "{alias}"
    );
    assert!(alias.contains("Members: "), "{alias}");

    let runtime = at("SignalConnection?").expect("the type");
    assert!(runtime.contains("type SignalConnection = {"), "{runtime}");
    assert!(runtime.contains("Disconnect"), "{runtime}");

    // A member named like the alias is the member's.
    assert_eq!(at("serde)"), None);
}

/// A name list over several lines reaches its module as a one-line
/// list does: a remote, a const and a function the list names hover as
/// their declarations, in the list and at a use. The path hovers as the
/// whole statement.
#[test]
fn a_list_over_several_lines_hovers_as_its_declarations() {
    use super::documents::{Recorder, alias_root};

    let main = "import {\n    Hit, -- the hit\n    LIMIT,\n    helper,\n} from \"./net\"\n\nHit.fire(helper(LIMIT))\n";
    let dir = alias_root(
        "multi-line-hover",
        &[
            ("alloy.toml", "[build]\nin = \"src\"\nout = \"build\"\n"),
            (
                "src/net.aly",
                "-- A hit.\nexport remote Hit(n: number) from client\n-- The cap.\nexport const LIMIT = 10\n-- Adds one.\nexport function helper(n: number): number\n    return n + 1\nend\n",
            ),
            ("src/main.aly", main),
        ],
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(Recorder(Arc::clone(&log))),
        Vec::new(),
        None,
    );

    {
        let mut st = server.state.lock().expect("state");
        st.root = Some(dir.clone());
        st.mirror = dir.join("mirror");
    }

    let uri = path_to_uri(&dir.join("src/main.aly"));
    server.open_doc(&uri, main.to_string(), 1, true);
    let hover = |needle: &str, occurrence: usize| {
        let at = main
            .match_indices(needle)
            .nth(occurrence)
            .expect("the name")
            .0;
        let (line, character) = position_of(main, at);
        let message = json!({ "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        } });
        log.lock().expect("the log").clear();
        server.source_binding_hover(&uri, &message, &json!(1));

        String::from_utf8_lossy(&log.lock().expect("the log")).into_owned()
    };

    for occurrence in [0, 1] {
        let sent = hover("Hit", occurrence);
        assert!(
            sent.contains("export remote Hit(n: number) from client"),
            "{sent}"
        );
        let sent = hover("LIMIT", occurrence);
        assert!(sent.contains("export const LIMIT: number"), "{sent}");
        let sent = hover("helper", occurrence);
        assert!(
            sent.contains("export function helper(n: number): number"),
            "{sent}"
        );
    }

    // The path hovers as the statement, as the source lays it out.
    let sent = hover("net", 0);
    assert!(sent.contains(r"import {\n    Hit, -- the hit\n"), "{sent}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A hover on a method name binds the receiver's type arguments, as
/// signature help does: `parts:take()` on a `Pool<Part>` reads
/// `take(): Part`, not the `T` of the impl.
#[test]
fn a_method_hover_binds_the_receiver_arguments() {
    let src = "struct Pool<T>\n  free: { T }\nend\n\nimpl Pool<T>\n  function take(self): T\n    return self.free[1]\n  end\nend\n\nlocal parts: Pool<Part> = new Pool { free = {} }\nprint(parts:take())\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let printed = "```alloy\nfunction Pool:take(): T\n```";

    assert_eq!(
        bind_hover_receiver(printed, doc, 11, 13).as_deref(),
        Some("```alloy\nfunction Pool:take(): Part\n```")
    );
    // Off a call there is no receiver to bind.
    assert_eq!(bind_hover_receiver(printed, doc, 10, 6), None);
}
