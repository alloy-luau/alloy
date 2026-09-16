use super::super::hover::{
    impl_self_type, intrinsic_code_home, member_doc, shadow_home, shadows_an_import, source_type,
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
    let mut text = crate::shapes::fold(&printed, &st.known_shapes_at(Some(uri)));

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
        "self: Slotted<T>"
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

    assert_eq!(hover_of(src, 1, 12, "local count: number"), "count: number");
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
    let src = "struct S as\n    x: number\n    private secret: number\nend\n\nimpl S as\n    function f(self): number\n        return self.secret + self.x\n    end\nend\n\nlocal s = new S { x = 1, secret = 2 }\n\nprint(s.x)\n";
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
    let known = crate::shapes::Known::default();
    let text = "function w.xs:map(function(x: number) return x end):find(self: {read number}, f: (number, number) -> boolean): number?";
    assert_eq!(
        crate::shapes::fold(text, &known),
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

    assert_eq!(hover_of(src, 4, 25, "local self: any"), "self: Shape");
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
    assert!(hover.contains("export struct Thing as"), "{hover}");
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
        Some("```luau\nb\n```")
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
A destructuring binding and a hint that says nothing.

`local { x, y } = t` lowers to `local _1 = t local x, y = _1.x, _1.y`.
The whole line is generated text, so the filter dropped every hint on it
and the names in the braces got none. The temp `_1` is nobody's name.

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

    // The shadow writes both names again, and the temp beside them.
    assert_eq!(
        destructured_name(doc, &hint(after("x,", 1), ": number")).as_deref(),
        Some("x")
    );
    assert_eq!(
        destructured_name(doc, &hint(after("y =", 1), ": string")).as_deref(),
        Some("y")
    );
    assert_eq!(destructured_name(doc, &hint(after("_1", 2), ": { }")), None);

    // After the mapping every hint of the line sits on its first byte.
    // The name each one carries says where it belongs.
    let mut hints = vec![
        json!({
            "kind": 1,
            "label": ": number",
            "position": { "line": 1, "character": 0 },
            DESTRUCTURED: "x",
        }),
        json!({
            "kind": 1,
            "label": ": string",
            "position": { "line": 1, "character": 0 },
            DESTRUCTURED: "y",
        }),
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

    // `x` sits at column 8 of the source line and `y` at column 11.
    assert_eq!(
        places,
        [(": number".to_string(), 9), (": string".to_string(), 12)],
        "{hints:?}"
    );
    assert!(!hints.iter().any(|h| h.get(DESTRUCTURED).is_some()));
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
    crate::shapes::fold_value(&mut hints, &st.known_shapes_at(Some(uri)));
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
    let text = crate::shapes::fold(&text, &st.known_shapes_at(Some(uri)));

    assert!(
        text.starts_with("```alloy\nexport const SCHEMA: {"),
        "{text}"
    );
    assert!(text.contains("default: number"), "{text}");
    assert!(!text.contains("typeof("), "{text}");
}
