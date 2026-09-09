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
