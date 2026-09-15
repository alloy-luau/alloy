use super::super::*;
use super::support::{files, one_file};

/// A word that begins a keyword drops the child's auto-imports.
/// `end` in a guard clause and in a one-line `impl` drew
/// `EncodingService`, which the editor sorted first.
#[test]
pub(crate) fn the_keyword_wins_over_an_auto_import() {
    let src = "impl T as end\nlocal function f(x: number?): number\n    if x == nil then return 0 end\n    return x\nend\n";
    let (st, uri) = one_file(src);
    let child = || {
        json!([
            {
                "label": "EncodingService",
                "kind": 7,
                "detail": "Auto-import",
                "sortText": "7",
                "additionalTextEdits": [{
                    "newText": "local EncodingService = game:GetService(\"EncodingService\")\n",
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                }],
            },
            { "label": "endsWith", "kind": 3, "detail": "Auto-import", "sortText": "7" },
            { "label": "elseif", "kind": 14, "sortText": "0" },
            { "label": "print", "kind": 3, "sortText": "4" },
        ])
    };
    let labels = |result: &Value| -> Vec<String> {
        result
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // `impl T as end`, the caret past `end`. The word is the whole
    // keyword and nothing else survives, so the list is empty and
    // the popup closes.
    let mut result = child();
    st.keyword_first(uri, 0, 13, &mut result);
    assert!(labels(&result).is_empty(), "{result}");

    // `if x == nil then return 0 end`, the caret past `end`.
    let mut result = child();
    st.keyword_first(uri, 2, 33, &mut result);
    assert!(labels(&result).is_empty(), "{result}");

    // Half a keyword keeps the keywords it begins, and no module.
    let mut result = child();
    st.keyword_first(uri, 2, 32, &mut result);
    let mut got = labels(&result);
    got.sort();
    assert_eq!(got, ["end", "enum"]);

    // A word that begins no keyword leaves the list alone.
    let mut result = child();
    st.keyword_first(uri, 3, 12, &mut result);
    assert_eq!(labels(&result).len(), 4);
}
/// `destroy` brings the timer form with it, and the three positions
/// the two words open each offer what belongs there.
#[test]
pub(crate) fn destroy_and_after_reach_the_completion() {
    let src = "local part = Instance.new(\"Part\")\ndes\n";
    let (st, uri) = one_file(src);
    let mut result = json!([{ "label": "print", "kind": 3, "sortText": "4" }]);
    st.keyword_first(uri, 1, 3, &mut result);
    let items = result.as_array().unwrap();
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"destroy"), "{labels:?}");

    let snippet = items
        .iter()
        .find(|i| i["label"] == json!("destroy x after n"))
        .expect("the timer form");
    assert_eq!(
        snippet["insertText"],
        json!("destroy ${1:value} after ${2:seconds}")
    );
    assert_eq!(snippet["insertTextFormat"], json!(2));
    assert_eq!(snippet["filterText"], json!("destroy"));

    // `after` is a statement keyword of its own.
    let src = "aft\n";
    let (st, uri) = one_file(src);
    let mut result = json!([{ "label": "print", "kind": 3, "sortText": "4" }]);
    st.keyword_first(uri, 0, 3, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();
    assert!(labels.contains(&"after"), "{labels:?}");

    // Past the operand of a `destroy`, only `after` belongs.
    assert_eq!(
        context_labels("local part = script.Part\ndestroy part \n", "destroy part "),
        ["after"]
    );

    // Past the seconds, the block opens with `do`, and `where` puts a
    // condition on it. A `where` already written leaves the `do`.
    assert_eq!(context_labels("after 3 \n", "after 3 "), ["do", "where"]);
    assert_eq!(
        context_labels(
            "local ready = true\nafter 3 where ready \n",
            "after 3 where ready "
        ),
        ["do"]
    );
}
/// The labels the context at the end of `head` offers.
fn context_labels(src: &str, head: &str) -> Vec<String> {
    let at = src.rfind(head).expect("the head") + head.len();
    let (st, uri) = one_file(src);
    let ctx = context::detect(src, at).expect("a context");

    st.context_items(uri, at, &ctx)
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect()
}
/// A whole keyword with more names behind it keeps the list, and
/// takes the first row. `else` is `elseif` as far as the letters go,
/// so the reader still needs to see both.
#[test]
pub(crate) fn a_whole_keyword_with_company_stays_in_the_list() {
    let src = "local elsewhere = 1\nif elsewhere == 1 then\nelse\n";
    let (st, uri) = one_file(src);
    let mut result = json!([
        { "label": "elsewhere", "kind": 6, "sortText": "4" },
        { "label": "print", "kind": 3, "sortText": "4" },
    ]);
    st.keyword_first(uri, 2, 4, &mut result);
    let mut labels: Vec<String> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect();
    labels.sort();
    assert_eq!(labels, ["else", "elseif", "elsewhere"]);

    let exact = result
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == json!("else"))
        .expect("the keyword");
    assert_eq!(exact["preselect"], json!(true));
    assert_eq!(exact["sortText"], json!("!else"));
}
/// Every ambient std name reaches an expression, and a package
/// module that carries one of those names arrives as an auto-import,
/// which is another row: `Signal` in `packages/` left the std
/// `Signal` out of the list.
#[test]
pub(crate) fn an_auto_import_does_not_hide_a_std_name() {
    let src = "local x = \n";
    let (st, uri) = one_file(src);
    let child = json!([
        { "label": "print", "kind": 3 },
        {
            "label": "Signal",
            "kind": 9,
            "detail": "Auto-import",
            "additionalTextEdits": [{ "newText": "local Signal = require(script.Signal)\n" }],
        },
    ]);
    let items = st.std_completions(uri, 0, 10, &child);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    for name in alloy::desugar::AMBIENT {
        assert!(labels.contains(name), "`{name}` is missing: {labels:?}");
    }

    // A name the child already answered stays the child's.
    let child = json!([{ "label": "Signal", "kind": 7 }]);
    let items = st.std_completions(uri, 0, 10, &child);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    assert!(!labels.contains(&"Signal"), "{labels:?}");
}
/// A type slot lists the std types, the type-only ones included.
#[test]
pub(crate) fn a_type_slot_lists_every_ambient_std_type() {
    let src = "local t: \n";
    let (st, uri) = one_file(src);
    let items = st.std_completions(uri, 0, 9, &json!([{ "label": "string" }]));
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    for name in alloy::desugar::AMBIENT_TYPES {
        assert!(labels.contains(name), "`{name}` is missing: {labels:?}");
    }
}
/// A `.` with no name after it stops the parse, so the child sees
/// the Alloy source and answers nothing. The std table holds the
/// members, and the type table offers its statics alone.
#[test]
pub(crate) fn a_dot_after_a_std_name_lists_the_std_members() {
    let src = "local h = HashMap.\nlocal f = Future.\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut result = Value::Null;
    complete_std_members(doc, 0, 18, true, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();

    assert_eq!(labels, ["new", "from"], "the statics of HashMap");
    assert_eq!(
        result[0]["detail"],
        json!("HashMap.new<K, V>(): HashMap<K, V>")
    );
    assert_eq!(result[0]["insertText"], json!("new()"));
    assert!(
        result[0]["documentation"]["value"]
            .as_str()
            .is_some_and(|v| v.starts_with("**HashMap.new**")),
        "{result}"
    );

    let mut result = Value::Null;
    complete_std_members(doc, 1, 17, true, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();

    for name in [
        "resolve",
        "reject",
        "delay",
        "all",
        "race",
        "any",
        "all_settled",
    ] {
        assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
    }

    assert!(!labels.contains(&"cancel"), "a method is no static");
}
/// A method takes a receiver, so the type table does not offer it;
/// a value does, and a static is no member of one.
#[test]
pub(crate) fn a_std_member_list_keeps_what_the_sigil_can_call() {
    let src = "local prices: HashMap<string, number> = HashMap.new()\nprices.\nHashMap.\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let child = json!([{ "label": "get" }, { "label": "new" }, { "label": "Fire" }]);
    let mut result = child.clone();
    complete_std_members(doc, 1, 7, true, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();

    assert!(labels.contains(&"get"), "{labels:?}");
    assert!(!labels.contains(&"new"), "a static is no member of a map");
    // A name the std table does not document stays the child's.
    assert!(labels.contains(&"Fire"), "{labels:?}");

    let mut result = child.clone();
    complete_std_members(doc, 2, 8, true, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();

    assert!(labels.contains(&"new"), "{labels:?}");
    assert!(!labels.contains(&"get"), "a method takes a receiver");
}
/// The child's member list gains the std's doc and signature.
#[test]
pub(crate) fn a_std_member_completion_carries_its_doc() {
    let src = "local prices: HashMap<string, number> = HashMap.new()\nprices:g\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut result = json!([{ "label": "get" }, { "label": "nothing" }]);
    attach_std_member_docs(&mut result, doc, 1, 8);

    assert_eq!(result[0]["detail"], json!("HashMap:get(key: K): V?"));
    assert!(
        result[0]["documentation"]["value"]
            .as_str()
            .is_some_and(|v| v.starts_with("**HashMap:get**")),
        "{result}"
    );
    assert!(
        result[1].get("detail").is_none(),
        "an unknown label is left"
    );
}
/// An `if` expression arm, a ternary, and a `default` get the
/// locals, the parameters, the file's own declarations, and the std
/// names. The child's own list wins wherever it answered.
#[test]
pub(crate) fn an_expression_position_the_child_leaves_empty_gets_the_scope() {
    let src = concat!(
        "struct Round as\n",
        "    seconds: number\n",
        "end\n",
        "\n",
        "export function pick(acc: number): string\n",
        "    local many = \"many\"\n",
        "    return if acc > 0 then \"a\" else \"b\"\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let at = src.find("then \"a\"").unwrap() + "then ".len();
    let (line, character) = position_of(src, at);
    let items = st.value_scope(uri, line, character, &json!([]));
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    for name in ["acc", "many", "pick", "Round", "Ok", "print", "if", "not"] {
        assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
    }

    // A field of a struct is no name the caret can write bare.
    assert!(!labels.contains(&"seconds"), "{labels:?}");
    // The child answered: its list already holds the scope.
    assert!(
        st.value_scope(uri, line, character, &json!([{ "label": "print" }]))
            .is_empty()
    );
}
/// A scrutinee the proxy cannot resolve keeps to the variants the
/// file declares or imports; another file's stay out.
#[test]
pub(crate) fn an_unresolved_scrutinee_offers_only_the_names_the_file_sees() {
    let src = concat!(
        "enum Phase as\n",
        "    Lobby\n",
        "    Playing\n",
        "end\n",
        "\n",
        "export function run(input: InputObject)\n",
        "    match input.KeyCode with\n",
        "        case \n",
        "    end\n",
        "end\n",
    );
    let (mut st, uri) = one_file(src);
    st.docs.insert(
        "file:///other.aly".to_string(),
        Doc::new(
            "export enum Coin as\n    Gold\n    Silver\nend\n".to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        ),
    );

    let at = src.find("case \n").unwrap() + "case ".len();
    let ctx = context::detect(src, at).expect("a case context");
    let items = st.context_items(uri, at, &ctx);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    for name in ["Lobby", "Playing", "Ok", "Err", "Enum", "default"] {
        assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
    }

    // `Coin` is another file's, and this one imports nothing.
    for name in ["Gold", "Silver"] {
        assert!(!labels.contains(&name), "`{name}` leaked: {labels:?}");
    }
}
/// A statement line inside a block takes `end`, and the member
/// column of an `impl` is the only place its member words belong.
#[test]
pub(crate) fn a_statement_line_in_a_block_takes_end() {
    let src = "export function f(n: number): number\n    local x = n\n    \nend\n";
    let (st, uri) = one_file(src);
    let at = src.find("\n    \n").unwrap() + 1 + 4;
    let (line, character) = position_of(src, at);
    let items = st.primitive_completions(uri, line, character, &json!([{ "label": "print" }]));
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"end"), "{labels:?}");
}
const MATCH_FILE: &str = concat!(
    "enum Msg as\n",
    "    Quit\n",
    "    Join(Player)\n",
    "end\n",
    "enum Color as Red, Green end\n",
    "type Answer = Result<number, string>\n",
    "local function handle(msg: Msg, tally: number, names: string[])\n",
    "    local parsed: Result<number, string> = Ok(1)\n",
    "    local seed = Msg.Join(p)\n",
    "    local reply: Answer = Ok(2)\n",
    "    local made = Array<Msg>()\n",
    "    match msg with\n",
    "        case \n",
    "    end\n",
    "end\n"
);
#[test]
pub(crate) fn a_scrutinee_resolves_to_a_type() {
    let (st, uri) = one_file(MATCH_FILE);
    let at = |name: &str| st.match_kind(uri, MATCH_FILE, MATCH_FILE.len(), name);
    let msg = MatchKind::Enum("Msg".to_string());

    // The annotation of a parameter, of a local, and of a const.
    assert_eq!(at("msg"), msg);
    assert_eq!(at("parsed"), MatchKind::Result);
    assert_eq!(at("names"), MatchKind::Array);
    assert_eq!(at("tally"), MatchKind::Literal);

    // The variant a local starts at.
    assert_eq!(at("seed"), msg);

    // A hover that names an enum or a `Result`.
    assert_eq!(at("Msg"), msg);
    assert_eq!(at("reply"), MatchKind::Result);

    // Anything else keeps the full list.
    assert_eq!(at("made"), MatchKind::Unknown);
    assert_eq!(at("p"), MatchKind::Unknown);
    assert_eq!(at("year % 4, year % 100"), MatchKind::Unknown);
}
/// The attribute list a `@` opens, for the declaration under it.
fn attribute_labels(src: &str) -> Vec<String> {
    let (st, uri) = one_file(src);
    let offset = src.find('@').expect("a sigil") + 1;
    let ctx = context::detect(src, offset).expect("an attribute list");

    st.context_items(uri, offset, &ctx)
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect()
}
/// An attribute belongs to what it sits above. The list offers the
/// ones that go there and no others, and a comment between the two
/// does not hide the declaration.
#[test]
pub(crate) fn an_attribute_list_follows_the_declaration_under_it() {
    let remote = attribute_labels("@\nremote Ping() from server\n");

    for name in ["@ratelimit", "@timeout", "@validate", "@unreliable"] {
        assert!(remote.contains(&name.to_string()), "{remote:?}");
    }

    for name in ["@derive", "@test", "@cfg", "@u8", "@rename", "@sealed"] {
        assert!(!remote.contains(&name.to_string()), "{remote:?}");
    }

    // The same list past a comment of either form.
    assert_eq!(
        attribute_labels("@\n-- why\nremote Ping() from server\n"),
        remote
    );
    assert_eq!(
        attribute_labels("@\n--[[ why\n   it stays ]]\nremote Ping() from server\n"),
        remote
    );

    let structure = attribute_labels("@\nstruct V as\n    x: number\nend\n");

    assert!(structure.contains(&"@derive".to_string()), "{structure:?}");
    assert!(structure.contains(&"@sealed".to_string()), "{structure:?}");
    assert!(
        !structure.contains(&"@ratelimit".to_string()),
        "{structure:?}"
    );

    let function = attribute_labels("@\nfunction go()\nend\n");

    for name in ["@native", "@checked", "@deprecated", "@test", "@cfg"] {
        assert!(function.contains(&name.to_string()), "{function:?}");
    }

    assert!(!function.contains(&"@derive".to_string()), "{function:?}");

    // A binding takes `@cfg` alone.
    assert_eq!(attribute_labels("@\nlocal count = 1\n"), ["@cfg"]);

    // The wire sizes go on a remote's parameter and a struct field.
    let param = attribute_labels("remote Hit(@ target: Player) from client\n");

    assert!(param.contains(&"@u8".to_string()), "{param:?}");
    assert!(!param.contains(&"@ratelimit".to_string()), "{param:?}");

    let field = attribute_labels("struct V as\n    @\n    x: number\nend\n");

    assert!(field.contains(&"@u8".to_string()), "{field:?}");
    assert!(field.contains(&"@rename".to_string()), "{field:?}");
    assert!(!field.contains(&"@derive".to_string()), "{field:?}");
}

/// A method takes what a function takes, `@test` apart: the runner
/// calls a test by name, and a method takes a receiver.
#[test]
pub(crate) fn an_attribute_on_a_method_leaves_test_out() {
    let method = attribute_labels(
        "struct W as\n    x: number\nend\nimpl W as\n    @\n    function grow(self): number\n        return self.x\n    end\nend\n",
    );

    assert!(method.contains(&"@native".to_string()), "{method:?}");
    assert!(method.contains(&"@inline".to_string()), "{method:?}");
    assert!(!method.contains(&"@test".to_string()), "{method:?}");

    // A declared attribute writes `on function`, which covers a method.
    let declared = attribute_labels(
        "attribute audited(why: string) on function\nstruct W as\n    x: number\nend\nimpl W as\n    @\n    function grow(self): number\n        return self.x\n    end\nend\n",
    );

    assert!(declared.contains(&"@audited".to_string()), "{declared:?}");

    // A plain function still takes `@test`.
    let function = attribute_labels("@\nfunction go()\nend\n");

    assert!(function.contains(&"@test".to_string()), "{function:?}");
}
/// Nothing under the caret to carry the attribute: the file ends, or
/// blank lines run to the end of it, or the line below starts no
/// declaration. Only the attributes that go anywhere read there; every
/// other one names a target the reader has not written yet.
#[test]
pub(crate) fn an_attribute_over_nothing_offers_the_ones_that_go_anywhere() {
    let open = ["@native", "@checked", "@deprecated"];
    let held_back = [
        "@derive",
        "@sealed",
        "@test",
        "@cfg",
        "@u8",
        "@f32",
        "@rename",
        "@skip",
        "@ratelimit",
        "@unreliable",
    ];

    for src in [
        // The end of the file.
        "@",
        "@\n",
        // Blank lines to the end of the file.
        "@\n\n\n",
        // A comment, and then nothing.
        "@\n-- later\n\n",
        // A line below that starts no declaration.
        "@\n\nprint(1)\n",
    ] {
        let labels = attribute_labels(src);
        assert_eq!(labels, open, "{src:?} gave {labels:?}");

        for name in held_back {
            assert!(!labels.contains(&name.to_string()), "{src:?}: {name}");
        }
    }

    // A declaration below still names the target, blank lines and all.
    let structure = attribute_labels("@\n\n\nstruct V as\n    x: number\nend\n");
    assert!(structure.contains(&"@derive".to_string()), "{structure:?}");
    assert!(!structure.contains(&"@u8".to_string()), "{structure:?}");

    // A declared attribute names its targets, so it waits for one too.
    let declared = attribute_labels("attribute audited(why: string) on remote\n\n@\n\n");
    assert_eq!(declared, open, "{declared:?}");
}
/// A declared attribute reaches the targets it names, and nothing
/// else.
#[test]
pub(crate) fn a_declared_attribute_reaches_its_own_targets() {
    let src = concat!(
        "attribute audited(reason: string) on remote, function\n",
        "\n",
        "@\n",
        "remote Ping() from server\n",
    );
    let (st, uri) = one_file(src);
    let offset = src.rfind('@').expect("a sigil") + 1;
    let ctx = context::detect(src, offset).expect("an attribute list");
    let labels: Vec<String> = st
        .context_items(uri, offset, &ctx)
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect();

    assert!(labels.contains(&"@audited".to_string()), "{labels:?}");

    let structure = concat!(
        "attribute audited(reason: string) on remote, function\n",
        "\n",
        "@\n",
        "struct V as\n    x: number\nend\n",
    );
    let (st, uri) = one_file(structure);
    let offset = structure.rfind('@').expect("a sigil") + 1;
    let ctx = context::detect(structure, offset).expect("an attribute list");
    let labels: Vec<String> = st
        .context_items(uri, offset, &ctx)
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect();

    assert!(!labels.contains(&"@audited".to_string()), "{labels:?}");
}
pub(crate) fn case_items(st: &State, uri: &str, src: &str) -> Vec<Value> {
    let offset = src.rfind("case ").unwrap() + "case ".len();
    let ctx = context::detect(src, offset).expect("a case list");

    st.context_items(uri, offset, &ctx)
}
#[test]
pub(crate) fn a_case_list_holds_the_arms_of_its_own_match() {
    let (st, uri) = one_file(MATCH_FILE);
    let items = case_items(&st, uri, MATCH_FILE);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();

    // The variants of `Msg` alone, then `default`. The other enum's
    // variants, `Ok`, `Err`, and `_` stay out.
    assert_eq!(labels, ["Quit", "Join", "default"]);
    assert_eq!(items[0]["textEdit"]["newText"], "Quit");
    assert_eq!(items[1]["textEdit"]["newText"], "Join($1)");
    assert_eq!(items[1]["insertTextFormat"], 2);
    assert_eq!(items[1]["detail"], "Msg.Join(Player)");
    assert!(
        items[1]["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("A variant of `enum Msg`")
    );
}
/// A struct scrutinee matches as one pattern over its fields. The list
/// once read the enum alone, so a struct got `Ok`, `Err`, and `Enum`.
#[test]
pub(crate) fn a_struct_scrutinee_takes_a_pattern_over_its_fields() {
    for src in [
        "struct Alpha as\n    x: number\n    y: number\nend\nlocal s: Alpha = new Alpha { x = 1, y = 2 }\nmatch s with\n    case \nend\n",
        "struct Alpha as\n    x: number\n    y: number\nend\nlocal s = new Alpha { x = 1, y = 2 }\nmatch s with\n    case \nend\n",
    ] {
        let (st, uri) = one_file(src);
        let items = case_items(&st, uri, src);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();

        assert_eq!(labels, ["Alpha { }", "_", "default"], "{src}");
        assert_eq!(items[0]["textEdit"]["newText"], "Alpha { ${1:x}, ${2:y} }");
        assert_eq!(items[0]["detail"], "Alpha { x, y }");
    }
}
/// An interface scrutinee takes one pattern for every struct whose
/// fields cover it. The list once held the enum variants of the file and
/// no struct at all.
#[test]
pub(crate) fn an_interface_scrutinee_takes_the_structs_that_satisfy_it() {
    let src = concat!(
        "interface Shape as\n",
        "    area: number\n",
        "end\n",
        "\n",
        "struct Circle as\n",
        "    r: number\n",
        "    area: number\n",
        "end\n",
        "\n",
        "struct Cat as\n",
        "    name: string\n",
        "end\n",
        "\n",
        "enum Suit as\n",
        "    Hearts\n",
        "end\n",
        "\n",
        "function describe(s: Shape): string\n",
        "    match s with\n",
        "        case \n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let items = case_items(&st, uri, src);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();

    // `Cat` covers no field of the interface, and `Hearts` is another
    // type's variant.
    assert_eq!(labels, ["Circle { }", "_", "default"], "{labels:?}");
    assert_eq!(
        items[0]["textEdit"]["newText"],
        "Circle { ${1:r}, ${2:area} }"
    );
    assert_eq!(items[0]["detail"], "Circle { r, area }");
}
#[test]
pub(crate) fn a_result_a_literal_and_an_array_take_their_own_arms() {
    let result = "local r: Result<number, string> = Ok(1)\nmatch r with\n    case \nend\n";
    let (st, uri) = one_file(result);
    let items = case_items(&st, uri, result);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(labels, ["Ok", "Err", "default"]);
    assert_eq!(items[0]["textEdit"]["newText"], "Ok(${1:v})");
    assert_eq!(items[1]["textEdit"]["newText"], "Err(${1:e})");

    let text = "local s: string = \"a\"\nmatch s with\n    case \nend\n";
    let (st, uri) = one_file(text);
    let items = case_items(&st, uri, text);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["label"], "default");

    let array = "local xs: string[] = {}\nmatch xs with\n    case \nend\n";
    let (st, uri) = one_file(array);
    let items = case_items(&st, uri, array);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(labels, ["[ first, ...rest ]", "[ ]", "default"]);
    assert_eq!(
        items[0]["textEdit"]["newText"],
        "[ ${1:first}, ...${2:rest} ]"
    );
}
/// `await X.m()` moves the receiver into a call the emit wrote, so
/// the child's own mapping lands past the member.
#[test]
pub(crate) fn an_awaited_receiver_keeps_its_member_list() {
    let src = "local function f(p: Future<number>)\n    local s = await Future.all(p)\nend\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).unwrap();
    let line = 1u32;
    let column = "    local s = await Future.".len() as u32;

    // The emit wrote `__alloy.await(__alloy.Future.all(p))`, and the
    // child's mapping no longer sits after `Future.`.
    assert!(!lands_on_member(doc, line, column, "Future", '.', 0));

    let shadow_line = doc.shadow.lines().nth(line as usize).unwrap();
    let at = context::member_column(
        doc.source.lines().nth(line as usize).unwrap(),
        shadow_line,
        "Future",
        context::Access::Plain,
        '.',
        0,
        column as usize,
    )
    .expect("a member column");
    assert!(shadow_line[..at].ends_with("Future."));

    // A plain access the emit copied keeps its own position.
    let plain = "local t = { a = 1 }\nlocal v = t.a\n";
    let (st, uri) = one_file(plain);
    let doc = st.docs.get(uri).unwrap();
    assert!(lands_on_member(
        doc,
        1,
        "local v = t.".len() as u32,
        "t",
        '.',
        0
    ));
}
/// A remote's surface follows the file's side and the declaration.
#[test]
pub(crate) fn a_remote_offers_the_members_its_side_reaches() {
    use alloy::directives::Side;

    let src = concat!(
        "@ratelimit(10, 1)\n",
        "export remote Chat(text: string) from client\n",
        "export remote Toast(message: string) from server\n",
        "export remote function Fetch(id: number) -> number from client\n"
    );
    let chat = remote_spec(src, "Chat").expect("Chat");
    let toast = remote_spec(src, "Toast").expect("Toast");
    let fetch = remote_spec(src, "Fetch").expect("Fetch");

    assert!(chat.ratelimited);
    assert!(!toast.ratelimited);
    assert!(fetch.answers);
    assert!(!chat.answers);

    // The client fires `Chat`; the server handles it.
    assert!(chat.holds("fire", Some(Side::Client)));
    assert!(!chat.holds("on", Some(Side::Client)));
    assert!(!chat.holds("call", Some(Side::Client)));
    assert!(chat.holds("on", Some(Side::Server)));
    assert!(chat.holds("on_ratelimited", Some(Side::Server)));

    // The server fires `Toast`, and only a server fire reaches all.
    assert!(toast.holds("fire_all", Some(Side::Server)));
    assert!(!toast.holds("fire_all", Some(Side::Client)));
    assert!(toast.holds("wait", Some(Side::Client)));
    assert!(!toast.holds("on_ratelimited", Some(Side::Server)));

    // A `remote function` answers, so the firing side may call it.
    assert!(fetch.holds("call", Some(Side::Client)));
    assert!(!fetch.holds("call", Some(Side::Server)));

    // A file with no side of its own sees both surfaces.
    assert!(chat.holds("fire", None) && chat.holds("on", None));
    assert!(!chat.holds("call", None));
}
/// The first line of the emit binds the module of each global under
/// `_g1`, and a module's own `global local` values under `_gs`. No
/// source writes either name, so no list offers one.
#[test]
pub(crate) fn the_global_module_temp_is_no_completion() {
    use super::super::completion::is_internal_name;

    assert!(is_internal_name("_g1"));
    assert!(is_internal_name("_g12"));
    assert!(is_internal_name("_gs"));
    assert!(!is_internal_name("_gold"));
    assert!(!is_internal_name("counter"));
}

/// A file sees its own declarations and what it imports, no more.
#[test]
pub(crate) fn a_type_list_holds_what_the_file_can_write() {
    let (st, uri) = one_file(MATCH_FILE);
    let labels: Vec<String> = st
        .type_completions(uri, &[])
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect();

    assert!(labels.contains(&"Msg".to_string()));
    assert!(labels.contains(&"Answer".to_string()));
    // The std traits a bound takes, and none of the std's own
    // numbered halves.
    assert!(labels.contains(&"Display".to_string()));
    for internal in ["Iter2", "Array3", "ResultMethods2", "Awaitable"] {
        assert!(!labels.contains(&internal.to_string()), "{internal}");
    }
}
/// The labels a type slot holds. `at` is the text the caret sits after.
fn type_slot_labels(st: &State, uri: &str, src: &str, at: &str) -> Vec<String> {
    let offset = src.find(at).expect(at) + at.len();
    let ctx = context::detect(src, offset).expect("a type slot");

    st.context_items(uri, offset, &ctx)
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect()
}

/// `stor: Scri|` offered no `Scribe`: the dotted path landed and the
/// name in front of the `.` reached no list. A bare type slot takes
/// every module and namespace a type hangs off, and the accept writes
/// the `.` so the path carries on.
#[test]
pub(crate) fn a_bare_type_slot_offers_the_head_of_a_path() {
    const SRC: &str = concat!(
        "import Scribe from \"./scribe\"\n",
        "import * as Star from \"./scribe\"\n",
        "namespace Shapes as\n",
        "    export type Box = { w: number }\n",
        "end\n",
        "namespace Funcs as\n",
        "    export function go() end\n",
        "end\n",
        "struct P as\n",
        "    stor: Scri\n",
        "end\n",
    );
    // The default import binds the module whole only when the module
    // returns one value, which the disk answers for.
    let dir = std::env::temp_dir().join(format!("alloy-prefix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");
    std::fs::write(
        dir.join("src/scribe.luau"),
        "export type Store = { id: number }\nlocal M = {}\nfunction M.open() end\nreturn M\n",
    )
    .expect("module");

    let main = dir.join("src/main.aly");
    std::fs::write(&main, SRC).expect("main");

    let uri = format!("file://{}", main.display());
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        snippets: true,
        ..State::default()
    };
    let options = EmitOptions {
        file_name: main.to_string_lossy().into_owned(),
        ..EmitOptions::default()
    };
    st.docs.insert(
        uri.clone(),
        Doc::new(
            SRC.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        ),
    );

    let uri = uri.as_str();
    let offset = SRC.find("Scri\n").expect("the slot") + "Scri".len();
    let ctx = context::detect(SRC, offset).expect("a type slot");
    let items = st.context_items(uri, offset, &ctx);
    let row = |name: &str| {
        items
            .iter()
            .find(|i| i["label"] == json!(name))
            .cloned()
            .unwrap_or(Value::Null)
    };

    for name in ["Scribe", "Star", "Shapes"] {
        let item = row(name);

        assert!(!item.is_null(), "{name}");
        // The name alone is half a type, so the accept writes the `.`
        // and asks the editor for the list under it.
        assert_eq!(item["textEdit"]["newText"], json!(format!("{name}.")));
        assert_eq!(
            item["command"]["command"],
            json!("editor.action.triggerSuggest")
        );
        assert_eq!(item["kind"], json!(9));
    }

    assert_eq!(row("Scribe")["detail"], json!("module"));
    assert_eq!(row("Star")["detail"], json!("module"));
    assert_eq!(row("Shapes")["detail"], json!("namespace"));
    // A prefix sorts with the plain types, by name.
    assert_eq!(row("Shapes")["sortText"], json!("1Shapes"));

    // A namespace of functions alone reaches no type, and a value of
    // the module is no type either.
    assert!(row("Funcs").is_null());
    assert!(row("open").is_null());

    // The emit flattens `Shapes.Box` to one name. No source writes it.
    assert!(row("Shapes_Box").is_null());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every shape that takes a type takes the head of a path too.
#[test]
pub(crate) fn every_type_slot_offers_the_head_of_a_path() {
    const SRC: &str = concat!(
        "import * as Star from \"./scribe\"\n",
        "namespace Shapes as\n",
        "    export type Box = { w: number }\n",
        "end\n",
        "struct P as\n",
        "    field: \n",
        "end\n",
        "function fp(a: )\n",
        "end\n",
        "function fr(): \n",
        "end\n",
        "local lx: \n",
        "type Ali = \n",
        "type Gen = Result<\n",
        "interface I as\n",
        "    mem: \n",
        "end\n",
        "interface J extends \n",
        "local sat = 1 satisfies \n",
        "impl \n",
    );
    let st = files(&[
        ("file:///t.aly", SRC),
        (
            "file:///scribe.luau",
            "export type Store = { id: number }\nlocal M = {}\nreturn M\n",
        ),
    ]);
    let uri = "file:///t.aly";
    let slots = [
        "    field: ",
        "fp(a: ",
        "fr(): ",
        "local lx: ",
        "type Ali = ",
        "Result<",
        "    mem: ",
        "extends ",
        "satisfies ",
        "impl ",
    ];

    for at in slots {
        let labels = type_slot_labels(&st, uri, SRC, at);

        assert!(labels.contains(&"Star".to_string()), "{at}");
        assert!(labels.contains(&"Shapes".to_string()), "{at}");
        assert!(!labels.contains(&"Shapes_Box".to_string()), "{at}");
    }
}

/// The emit writes `Shapes_Box` for a type inside a namespace, and the
/// declaration index holds that name so a hover on the artifact reads.
/// No list offers it, in a type slot or an enum payload.
#[test]
pub(crate) fn no_type_list_offers_a_folded_name() {
    const SRC: &str = concat!(
        "namespace Shapes as\n",
        "    export type Box = { w: number }\n",
        "    export namespace Deep as\n",
        "        export type Ray = { d: number }\n",
        "    end\n",
        "end\n",
        "enum E as\n",
        "    One(\n",
        "end\n",
    );
    let (st, uri) = one_file(SRC);
    let labels: Vec<String> = st
        .type_completions(uri, &[])
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect();

    for folded in ["Shapes_Box", "Shapes_Deep_Ray"] {
        assert!(!labels.contains(&folded.to_string()), "{folded}");
    }

    // The hover still finds the member under the name the emit wrote.
    let doc = st.docs.get(uri).expect("the document");
    assert!(doc.decls.iter().any(|d| d.name == "Shapes_Box"));

    let payload = type_slot_labels(&st, uri, SRC, "One(");
    assert!(!payload.contains(&"Shapes_Box".to_string()));
}

/// A nested namespace is one more step of the path: `Shapes.` offers
/// `Deep`, and accepting it writes the `.` that `Shapes.Deep.Ray`
/// needs.
#[test]
pub(crate) fn a_nested_namespace_continues_the_path() {
    const SRC: &str = concat!(
        "namespace Shapes as\n",
        "    export type Box = { w: number }\n",
        "    export namespace Deep as\n",
        "        export type Ray = { d: number }\n",
        "    end\n",
        "end\n",
        "local a: Shapes.\n",
        "local b: Shapes.Deep.\n",
    );
    let (st, uri) = one_file(SRC);
    let at = |head: &str| {
        let offset = SRC.find(head).expect(head) + head.len();
        let ctx = context::detect(SRC, offset).expect("a type slot");

        st.context_items(uri, offset, &ctx)
    };
    let outer = at("local a: Shapes.");
    let row = |items: &[Value], name: &str| {
        items
            .iter()
            .find(|i| i["label"] == json!(name))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let deep = row(&outer, "Deep");

    assert_eq!(deep["detail"], json!("namespace Shapes.Deep"));
    assert_eq!(deep["textEdit"]["newText"], json!("Deep."));
    assert_eq!(
        deep["command"]["command"],
        json!("editor.action.triggerSuggest")
    );

    // A type is the end of the path and writes its name alone.
    assert_eq!(row(&outer, "Box")["textEdit"]["newText"], json!("Box"));
    assert!(row(&outer, "Box")["command"].is_null());

    // The step lands on the types under it.
    let inner = at("local b: Shapes.Deep.");
    let labels: Vec<String> = inner
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(labels, ["Ray"]);
}

/// A struct literal lists the fields of its struct, and hides the
/// private ones outside the impl.
#[test]
pub(crate) fn a_struct_literal_lists_its_own_fields() {
    let src = concat!(
        "struct Round as\n",
        "    public phase: Phase\n",
        "    private ready: number\n",
        "end\n",
        "local r = new Round { \n"
    );
    let (st, uri) = one_file(src);
    let offset = src.rfind("{ ").unwrap() + 2;
    let ctx = context::detect(src, offset).expect("a field slot");
    let items = st.context_items(uri, offset, &ctx);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(labels, ["phase"]);
    assert_eq!(items[0]["textEdit"]["newText"], "phase = ${1:phase}");
}
/// A signature reads its parameters, `->` and all.
#[test]
pub(crate) fn a_signature_drops_its_receiver_and_names_its_arguments() {
    assert_eq!(
        drop_receiver("({ next: (any) -> number? }, (number) -> boolean) -> boolean"),
        Some("((number) -> boolean) -> boolean".to_string())
    );
    assert_eq!(
        call_snippet("earn", "(self: Profile, amount: number) -> number"),
        Some("earn(${1:self}, ${2:amount})$0".to_string())
    );
    assert_eq!(
        call_snippet("alive", "(Profile) -> boolean"),
        Some("alive(${1:Profile})$0".to_string())
    );
    assert_eq!(
        call_snippet("history", "() -> string[]"),
        Some("history()".to_string())
    );
    // A vararg fills no slot of its own.
    assert_eq!(
        call_snippet("flush", "(...any) -> { Event }"),
        Some("flush()".to_string())
    );
    assert_eq!(plain_snippet("Score($1, $2)"), "Score()");
}
/// A derived table pair prints no field the struct keeps private.
#[test]
pub(crate) fn a_derived_table_hides_the_private_fields() {
    let private: HashSet<String> = ["coins", "log"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        hide_record(
            "(Profile) -> { coins: number, id: number, log: string[], name: string }",
            &private
        ),
        "(Profile) -> { id: number, name: string }"
    );
}
#[test]
pub(crate) fn an_editor_without_snippets_takes_the_plain_text() {
    let (mut st, uri) = one_file(MATCH_FILE);
    st.snippets = false;
    let items = case_items(&st, uri, MATCH_FILE);
    assert_eq!(items[1]["textEdit"]["newText"], "Join()");
    assert!(items[1].get("insertTextFormat").is_none());
    assert_eq!(
        plain_snippet("[ ${1:first}, ...${2:rest} ]"),
        "[ first, ...rest ]"
    );
}
#[test]
pub(crate) fn import_temps_leave_the_type_names() {
    let shadow = "local _1 = require(\"./inventory\") local add = _1.add\n_2 = require(\"./x\")\nlocal m = require(\"./m\")\n";
    assert_eq!(import_temps(shadow), ["_1", "_2"]);
    let mut v = json!({ "contents": { "value": "function total(inv: _1.Inventory): _2.Item" } });
    strip_import_temps(&mut v, shadow);
    assert_eq!(
        v["contents"]["value"],
        "function total(inv: Inventory): Item"
    );
}
#[test]
pub(crate) fn a_variant_signature_splits_into_its_payload_types() {
    assert_eq!(
        payload_types("Msg.Move(Player, number)"),
        vec!["Player", "number"]
    );
    assert_eq!(
        payload_types("Msg.Pair({ x: number, y: number }, Map<string, number>)"),
        vec!["{ x: number, y: number }", "Map<string, number>"]
    );
    assert!(payload_types("Msg.Quit").is_empty());
    assert!(payload_types("Msg.Unit()").is_empty());
}
#[test]
pub(crate) fn a_colon_call_drops_the_receiver() {
    assert_eq!(
        drop_receiver("(Account, number) -> number").as_deref(),
        Some("(number) -> number")
    );
    assert_eq!(
        drop_receiver("({read number}) -> number?").as_deref(),
        Some("() -> number?")
    );
    assert_eq!(
        drop_receiver("<U>(read number[], (number, number) -> U) -> U[]").as_deref(),
        Some("<U>((number, number) -> U) -> U[]")
    );
}
#[test]
pub(crate) fn a_private_field_leaves_the_constructor_signature() {
    let private: HashSet<String> = ["token".to_string()].into_iter().collect();
    assert_eq!(
        hide_private("({ name: string, token: string? }) -> Cfg", &private),
        "({ name: string }) -> Cfg"
    );
}

/// `player.` names a value: the constructor belongs to `Player.new`,
/// and the emit's metatable is the only reason the child offered it.
#[test]
pub(crate) fn a_value_offers_no_constructor() {
    let src = "struct Player as\n    read name: string\nend\n\nlocal player = new Player { name = \"a\" }\nprint(player.name)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let child = || {
        json!([
            { "label": "new", "kind": 3, "detail": "({ name: string }) -> Player" },
            { "label": "name", "kind": 5, "detail": "string" },
        ])
    };
    let labels = |result: &Value| -> Vec<String> {
        result
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // `player.` on line 5, past the dot.
    let mut result = child();
    clean_completion(&mut result, doc, 5, 13, true);
    assert_eq!(labels(&result), ["name"]);

    // `Player.` names the type, and the constructor stays.
    let src = "struct Player as\n    read name: string\nend\n\nlocal p = Player.new({ name = \"a\" })\nprint(p)\n";
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let mut result = child();
    clean_completion(&mut result, doc, 4, 17, true);
    let mut got = labels(&result);
    got.sort();
    assert_eq!(got, ["name", "new"]);
}

/// A trait has no table in the emit, so the child has no type for
/// `self` inside a default method. The trait's own signatures are the
/// list.
#[test]
fn self_inside_a_trait_lists_the_trait_methods() {
    let src = "trait T as\n    function f(self): number\n\n    function g(self): number\n        return self:\n    end\nend\n";
    let (st, uri) = one_file(src);
    let items = st.trait_self_members(uri, 4, 20);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert_eq!(labels, vec!["f", "g"]);
    assert_eq!(items[0]["detail"], "function T(self): number");

    // Outside the trait, and after a name that is not `self`, nothing.
    assert!(st.trait_self_members(uri, 6, 0).is_empty());
}

/// An index before the member: `profile["a"].`, `map?[k].` and
/// `parts![1].`. The emit moves the receiver of the two guarded forms,
/// so the child's own mapping no longer sits after the separator, and
/// the column the lowering wrote is the one that answers.
#[test]
pub(crate) fn an_index_before_a_member_finds_its_column() {
    let head = concat!(
        "type P = { name: string }\n",
        "local map: { [string]: P }? = nil\n",
        "local plain: { [string]: P } = {}\n",
    );

    for (line, want) in [
        ("local a = plain[\"k\"].na", "plain[\"k\"]."),
        ("local a = map?[\"k\"].na", "map[\"k\"]."),
        ("local a = map![\"k\"].na", "map)[\"k\"]."),
    ] {
        let src = format!("{head}{line}\nprint(a)\n");
        let (st, uri) = one_file(&src);
        let doc = st.docs.get(uri).unwrap();
        let no = 3u32;
        let column = line.len() as u32;
        let (base, access, sep, prefix) =
            context::member_at(&doc.source, doc.source.find(".na").unwrap() + 3)
                .unwrap_or_else(|| panic!("{line}"));
        assert_eq!(prefix, 2, "{line}");

        let (shadow_no, _) = doc.to_shadow(no, column);
        let shadow_line = doc.shadow.lines().nth(shadow_no as usize).unwrap();
        let at = context::member_column(
            doc.source.lines().nth(no as usize).unwrap(),
            shadow_line,
            &base,
            access,
            sep,
            prefix,
            column as usize,
        )
        .unwrap_or_else(|| panic!("{line}: {shadow_line}"));
        assert!(
            shadow_line[..at - prefix].ends_with(want),
            "{line}: {shadow_line}"
        );
    }
}

/// `plain["k"]?.` binds the index to a name of the lowering's own, the
/// way a call before a guard does, so the member follows the branch.
#[test]
pub(crate) fn an_index_before_a_guard_follows_the_branch() {
    let src = concat!(
        "type P = { name: string }\n",
        "local plain: { [string]: P } = {}\n",
        "local a = plain[\"k\"]?.na\nprint(a)\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).unwrap();
    let offset = doc.source.find(".na").unwrap() + 3;
    let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let (shadow_no, _) = doc.to_shadow(2, (offset - line_start) as u32);
    let shadow_line = doc.shadow.lines().nth(shadow_no as usize).unwrap();

    // No receiver of the source stands on the lowered line.
    let (base, access, sep, prefix) = context::member_at(&doc.source, offset).expect("a member");
    assert_eq!(base, "plain[\"k\"]");
    assert_eq!(
        context::member_column(
            doc.source.lines().nth(2).unwrap(),
            shadow_line,
            &base,
            access,
            sep,
            prefix,
            offset - line_start,
        ),
        None
    );

    let at = context::guarded_member_column(&doc.source[line_start..offset], shadow_line, sep)
        .unwrap_or_else(|| panic!("{shadow_line}"));
    assert!(shadow_line[..at].ends_with('.'), "{shadow_line}");
}

/// `profile["|` and `profile[|`: the keys the receiver's type names,
/// each written with its quotes. `map[|` on an index signature names
/// none, so the scope the child lists stands.
#[test]
pub(crate) fn an_open_index_offers_the_keys_of_its_receiver() {
    let head = concat!(
        "type P = { name: string, coins: number }\n",
        "local profile: P = { name = \"a\", coins = 1 }\n",
        "local po: P? = nil\n",
        "local map: { [string]: number } = {}\n",
    );

    for (line, want) in [
        ("local v = profile[", vec!["\"name\"", "\"coins\""]),
        ("local v = profile[\"", vec!["\"name\"", "\"coins\""]),
        ("local v = profile[\"na", vec!["\"name\"", "\"coins\""]),
        ("local v = po?[", vec!["\"name\"", "\"coins\""]),
        ("local v = po![", vec!["\"name\"", "\"coins\""]),
        ("local v = map[", vec![]),
        ("local v = profile[i + ", vec![]),
    ] {
        let src = format!("{head}{line}\n");
        let (st, uri) = one_file(&src);
        let offset = src.len() - 1;
        let items = match context::detect(&src, offset) {
            Some(ctx) => st.context_items(uri, offset, &ctx),
            None => Vec::new(),
        };
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels, want, "{line}");
    }

    // The item replaces the quote the author opened, so the key never
    // carries two.
    let src = format!("{head}local v = profile[\"na\n");
    let (st, uri) = one_file(&src);
    let offset = src.len() - 1;
    let ctx = context::detect(&src, offset).expect("an index key");
    let items = st.context_items(uri, offset, &ctx);
    assert_eq!(items[0]["textEdit"]["newText"], "\"name\"");
    assert_eq!(
        items[0]["textEdit"]["range"]["start"]["character"],
        json!("local v = profile[".len())
    );
}

/// A module that ends in `return <expr>` has no export table: its value
/// is the module. A name in braces completes to a key of that value,
/// and the head of the import offers the name the module returns.
#[test]
pub(crate) fn an_import_of_a_returning_module_completes_its_keys() {
    let dir = std::env::temp_dir().join(format!("alloy-returning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");
    std::fs::write(
        dir.join("src/palette.aly"),
        "--- The colours the UI paints with.\nlocal Palette = { dark = \"#111111\" }\n\n--- Lightens a hex colour.\nfunction Palette.tint(hex: string): string\n    return hex\nend\n\nreturn Palette\n",
    )
    .expect("module");

    let src = "import { } from \"./palette\"\nimport  from \"./palette\"\n";
    let main = dir.join("src/main.aly");
    std::fs::write(&main, src).expect("main");

    let uri = format!("file://{}", main.display());
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        snippets: true,
        ..State::default()
    };
    let options = EmitOptions {
        file_name: main.to_string_lossy().into_owned(),
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
    let labels = |at: usize| -> Vec<String> {
        let ctx = context::detect(src, at).expect("a context");

        st.context_items(&uri, at, &ctx)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    };

    // In braces: the keys of the value the module returns.
    let keys = labels(src.find("} from").expect("the braces"));
    assert!(keys.contains(&"dark".to_string()), "{keys:?}");
    assert!(keys.contains(&"tint".to_string()), "{keys:?}");

    // At the head: the name the module returns is what a bare import
    // binds, so it reads best there.
    let head = labels(src.find("import  from").expect("the head") + "import ".len());
    assert!(head.contains(&"Palette".to_string()), "{head:?}");

    // The module's own text reaches the importer, so a hover on a name
    // it took reads the declaration and the comment above it.
    let sources = &st.docs[&uri].import_sources;
    assert!(
        sources
            .iter()
            .any(|t| t.contains("function Palette.tint") && t.contains("Lightens a hex colour")),
        "{sources:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A declaration another file keeps to itself reaches no list. The
/// server's `struct Test` is neither exported and imported nor
/// `global`, so the client file, which declares a `namespace Test` of
/// its own, reads its own name.
#[test]
fn a_name_another_file_keeps_reaches_no_list() {
    let st = files(&[
        (
            "file:///src/client/main.client.aly",
            "namespace Test as\n    public const test = 1\nend\n\nlocal x = Tes\n",
        ),
        (
            "file:///src/server/main.server.aly",
            "struct Test as\n    x: number\nend\n",
        ),
    ]);
    // The child sees the local the emit wrote for the namespace.
    let mut result = json!([{ "label": "Test", "kind": 6 }]);
    st.mark_declarations("file:///src/client/main.client.aly", &mut result);
    let item = &result[0];

    assert_eq!(item["detail"], json!("namespace Test"));
    assert_eq!(item["kind"], json!(9));

    // The other file's struct never names it.
    let mut theirs = json!([{ "label": "Test", "kind": 6 }]);
    st.mark_declarations("file:///src/server/main.server.aly", &mut theirs);
    assert_eq!(theirs[0]["detail"], json!("struct Test"));
}

/// The detail names the kind before the name, for every declaration
/// the summaries carry.
#[test]
fn a_declaration_completes_as_its_kind() {
    let src = "export struct Profile as\n    name: string\nend\nenum Msg as\n    Leave\nend\ntrait Show as\n    function show(self): string\nend\ninterface Named as\n    name: string\nend\ntype Id = number\nnamespace Util as\n    const K = 1\nend\n";
    let (st, uri) = one_file(src);
    let mut result = json!([
        { "label": "Profile", "kind": 6 },
        { "label": "Msg", "kind": 6 },
        { "label": "Show", "kind": 6 },
        { "label": "Named", "kind": 6 },
        { "label": "Id", "kind": 6 },
        { "label": "Util", "kind": 6 },
    ]);
    st.mark_declarations(uri, &mut result);
    let details: Vec<&str> = result
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["detail"].as_str().unwrap_or(""))
        .collect();

    assert_eq!(
        details,
        [
            "struct Profile",
            "enum Msg",
            "trait Show",
            "interface Named",
            "type Id",
            "namespace Util",
        ]
    );
}

/// The `{ }` of `new Instance("Part")` lists each property with the
/// type it writes, the way a member list after `part.` reads.
#[test]
fn an_object_initializer_names_the_type_of_each_property() {
    let src = "local part = new Instance(\"Part\") {\n    \n}\n";
    let (st, uri) = one_file(src);
    let at = src.find("\n    \n").expect("the body") + "\n    ".len();
    let ctx = context::detect(src, at).expect("a context");
    let items = st.context_items(uri, at, &ctx);
    let detail = |label: &str| -> String {
        items
            .iter()
            .find(|i| i["label"] == label)
            .and_then(|i| i["detail"].as_str().map(str::to_string))
            .unwrap_or_default()
    };

    assert_eq!(detail("Size"), "Vector3");
    assert_eq!(detail("Name"), "string");
    assert_eq!(detail("Anchored"), "boolean");
    // The dump spells an enum `EnumMaterial`; the reader writes it with
    // the dot.
    assert_eq!(detail("Material"), "Enum.Material");
    assert_eq!(detail("Touched"), "event of Part");
}

/// With a `--docs` file the initializer carries the engine's own text,
/// the way the child's member list does.
#[test]
fn an_object_initializer_carries_the_class_documentation() {
    let dir = std::env::temp_dir().join(format!("alloy-api-docs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("api-docs.json");
    std::fs::write(
        &path,
        "{\"@roblox/globaltype/BasePart.Size\": { \"documentation\": \"The dimensions of a <code>Part</code>.\", \"learn_more_link\": \"https://example.test/Size\", \"code_sample\": \"part.Size = Vector3.new(1, 1, 1)\" }}",
    )
    .unwrap();

    let src = "local part = new Instance(\"Part\") {\n    \n}\n";
    let (mut st, uri) = one_file(src);
    st.api_docs = Some(path);
    let at = src.find("\n    \n").expect("the body") + "\n    ".len();
    let ctx = context::detect(src, at).expect("a context");
    let items = st.context_items(uri, at, &ctx);
    let size = items.iter().find(|i| i["label"] == "Size").expect("Size");

    // The text the way luau-lsp writes it: the description, the link,
    // and the sample, so the editor's side panel reads the same for
    // a property the proxy lists and one the child lists.
    assert_eq!(
        size["documentation"]["value"],
        json!(
            "The dimensions of a `Part`.\n\n[Learn More](https://example.test/Size)\n\n```luau\npart.Size = Vector3.new(1, 1, 1)\n```"
        )
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A property the engine deprecates carries the tag the editor strikes
/// the row through with, and keeps the note in its documentation. The
/// child marks its own rows; the object initializer is the proxy's.
#[test]
fn a_deprecated_property_carries_the_tag() {
    let dir = std::env::temp_dir().join(format!("alloy-deprecated-docs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("api-docs.json");
    std::fs::write(
        &path,
        concat!(
            "{\"@roblox/globaltype/BasePart.brickColor\": { \"documentation\": ",
            "\"<strong>Deprecated:</strong> use <code>BrickColor</code>.\", ",
            "\"learn_more_link\": \"\", \"code_sample\": \"\" },",
            "\"@roblox/globaltype/BasePart.BrickColor\": { \"documentation\": ",
            "\"Determines the color of a part.\", \"learn_more_link\": \"\", \"code_sample\": \"\" }}",
        ),
    )
    .unwrap();

    let src = "local part = new Instance(\"Part\") {\n    \n}\n";
    let (mut st, uri) = one_file(src);
    st.api_docs = Some(path);
    let at = src.find("\n    \n").expect("the body") + "\n    ".len();
    let ctx = context::detect(src, at).expect("a context");
    let mut result = json!(st.context_items(uri, at, &ctx));
    st.deprecated_pass(uri, &mut result);
    let row = |label: &str| -> Value {
        result
            .as_array()
            .expect("a list")
            .iter()
            .find(|i| i["label"] == label)
            .cloned()
            .unwrap_or_default()
    };

    assert_eq!(
        row("brickColor")["tags"],
        json!([1]),
        "{}",
        row("brickColor")
    );
    assert!(
        row("brickColor")["documentation"]["value"]
            .as_str()
            .is_some_and(|t| t.starts_with("Deprecated:")),
        "{}",
        row("brickColor")
    );
    // The current spelling is a row like any other.
    assert_eq!(row("BrickColor").get("tags"), None);

    // The setting leaves the deprecated spelling out of the list.
    st.editor.hide_roblox_deprecated = true;
    let mut result = json!(st.context_items(uri, at, &ctx));
    st.deprecated_pass(uri, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();

    assert!(!labels.contains(&"brickColor"), "{labels:?}");
    assert!(labels.contains(&"BrickColor"), "{labels:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The two settings: one hides what the Roblox API deprecates, the
/// other hides what the source marks as well. A method the file
/// declares on a foreign type is the author's own, so it stays.
#[test]
fn the_settings_hide_the_deprecated_rows() {
    let src = concat!(
        "@deprecated(\"use fresh\")\n",
        "local function old() end\n",
        "\n",
        "local function fresh() end\n",
        "\n",
        "impl BasePart as\n",
        "    public function destroy(self) end\n",
        "end\n",
    );
    let child = || {
        json!([
            // The child marks its own rows with the older flag.
            { "label": "old", "kind": 3, "deprecated": true },
            { "label": "fresh", "kind": 3 },
            { "label": "brickColor", "kind": 5,
              "documentation": { "kind": "markdown", "value": "Deprecated: use `BrickColor`." } },
            { "label": "Size", "kind": 5 },
            { "label": "destroy", "kind": 3, "deprecated": true },
        ])
    };
    let labels = |result: &Value| -> Vec<String> {
        result
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    };
    let (mut st, uri) = one_file(src);
    st.extensions = vec![alloy::extensions::Extension {
        target: "BasePart".to_string(),
        name: "destroy".to_string(),
        is_static: false,
        params: String::new(),
        ret: None,
    }];

    // Both settings off: every row stays, and each deprecated one
    // carries the tag.
    let mut result = child();
    st.deprecated_pass(uri, &mut result);
    assert_eq!(
        labels(&result),
        ["old", "fresh", "brickColor", "Size", "destroy"]
    );
    let tags = |result: &Value, label: &str| -> Value {
        result
            .as_array()
            .expect("a list")
            .iter()
            .find(|i| i["label"] == label)
            .and_then(|i| i.get("tags").cloned())
            .unwrap_or(Value::Null)
    };
    assert_eq!(tags(&result, "old"), json!([1]));
    assert_eq!(tags(&result, "brickColor"), json!([1]));
    assert_eq!(tags(&result, "fresh"), Value::Null);

    // The engine's own deprecated member goes; the author's `old` and
    // the `destroy` this file declares stay.
    st.editor.hide_roblox_deprecated = true;
    let mut result = child();
    st.deprecated_pass(uri, &mut result);
    assert_eq!(labels(&result), ["old", "fresh", "Size", "destroy"]);

    // With the second setting the author's own `@deprecated` goes too,
    // and `destroy`, which the file declares without the attribute,
    // still stands.
    st.editor.hide_all_deprecated = true;
    let mut result = child();
    st.deprecated_pass(uri, &mut result);
    assert_eq!(labels(&result), ["fresh", "Size", "destroy"]);

    // A hidden name is still a name of the file: hover and go to
    // definition read the declarations, which the filter never touches.
    assert!(
        st.docs
            .get(uri)
            .is_some_and(|d| d.source.contains("function old"))
    );
}

/// `hideAllDeprecated` read the attribute in the open file alone, so an
/// imported `@deprecated` function stayed in the list. The modules the
/// file imports mark their own rows now, namespace members among them.
#[test]
pub(crate) fn an_imported_deprecated_name_hides_under_the_setting() {
    const DEP: &str = concat!(
        "@deprecated(\"use newFn instead\")\n",
        "export function oldFn(): number\n",
        "    return 1\n",
        "end\n",
        "\n",
        "export function newFn(): number\n",
        "    return 2\n",
        "end\n",
        "\n",
        "export namespace Old as\n",
        "    @deprecated\n",
        "    public function f() end\n",
        "end\n",
    );
    const USE: &str = "import { oldFn, newFn, Old } from \"./dep\"\n\nlocal x = old\n";
    let mut st = super::support::files(&[("file:///dep.aly", DEP), ("file:///use.aly", USE)]);
    st.docs
        .get_mut("file:///use.aly")
        .expect("the file")
        .import_sources
        .push(DEP.to_string());
    let child = || {
        json!([
            { "label": "oldFn", "kind": 3 },
            { "label": "newFn", "kind": 3 },
            { "label": "f", "kind": 3 },
        ])
    };
    let labels = |result: &Value| -> Vec<String> {
        result
            .as_array()
            .expect("items")
            .iter()
            .map(|i| i["label"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    // Without the setting the rows stay, and the imported ones carry
    // the tag.
    let mut result = child();
    st.deprecated_pass("file:///use.aly", &mut result);
    assert_eq!(labels(&result), ["oldFn", "newFn", "f"]);
    assert_eq!(result[0]["tags"], json!([1]));
    assert_eq!(result[1]["tags"], Value::Null);
    assert_eq!(result[2]["tags"], json!([1]));

    st.editor.hide_all_deprecated = true;
    let mut result = child();
    st.deprecated_pass("file:///use.aly", &mut result);
    assert_eq!(labels(&result), ["newFn"]);
}

/// The literals an attribute argument takes: the members of a narrowed
/// union, and the variants of an enum.
#[test]
pub(crate) fn an_attribute_argument_completes_its_own_literals() {
    let items = |src: &str, head: &str| -> Vec<(String, String)> {
        let at = src.rfind(head).expect("the head") + head.len();
        let (st, uri) = one_file(src);
        let ctx = context::detect(src, at).expect("an argument context");

        st.context_items(uri, at, &ctx)
            .iter()
            .map(|i| {
                (
                    i["label"].as_str().unwrap_or_default().to_string(),
                    i["textEdit"]["newText"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect()
    };
    let enums = "enum Lifecycle as\n    Init\n    Start\nend\n";

    // An enum-typed list: the variants, written the way a source writes
    // them.
    let src = format!(
        "{enums}attribute provider(lifecycles: Lifecycle[]) on impl as\n    requires private function each lifecycles(self)\nend\n\n@provider({{ lifecycles = [  ] }})\nimpl S as\nend\n"
    );
    assert_eq!(
        items(&src, "lifecycles = [ "),
        [
            ("Lifecycle.Init".to_string(), "Lifecycle.Init".to_string()),
            ("Lifecycle.Start".to_string(), "Lifecycle.Start".to_string()),
        ]
    );

    // A narrowed union of strings: the members, in quotes outside a
    // string and bare inside one.
    let src = "attribute phases(names: (\"init\" | \"start\")[]) on impl\n\n@phases({ names = [  ] })\nimpl S as\nend\n";
    assert_eq!(
        items(src, "names = [ "),
        [
            ("init".to_string(), "\"init\"".to_string()),
            ("start".to_string(), "\"start\"".to_string()),
        ]
    );

    let src = "attribute phases(names: (\"init\" | \"start\")[]) on impl\n\n@phases({ names = [ \"in ] })\nimpl S as\nend\n";
    assert_eq!(
        items(src, "[ \"in"),
        [
            ("init".to_string(), "init".to_string()),
            ("start".to_string(), "start".to_string()),
        ]
    );

    // A single value takes the same list.
    let src = format!("{enums}attribute one(stage: Lifecycle) on impl\n\n@one()\nimpl S as\nend\n");
    assert_eq!(items(&src, "@one(").len(), 2);

    // A parameter with no narrowed type offers nothing: an argument is a
    // literal, so no name from the scope belongs here.
    let src = "attribute k(n: number) on impl\n\n@k()\nimpl S as\nend\n";
    assert!(items(src, "@k(").is_empty());

    // A built-in attribute has no declaration to read, and the list is
    // empty rather than the whole scope.
    assert!(items("@ratelimit()\nremote P() from server\n", "@ratelimit(").is_empty());
}

/// The member column of a declaration under a contract offers what the
/// contract asks for, first.
#[test]
pub(crate) fn a_contract_offers_the_members_it_requires() {
    let rows = |src: &str, head: &str| -> Vec<(String, String, String)> {
        let at = src.rfind(head).expect("the head") + head.len();
        let (st, uri) = one_file(src);
        let ctx = context::detect(src, at).expect("a member column");

        st.context_items(uri, at, &ctx)
            .iter()
            .map(|i| {
                (
                    i["label"].as_str().unwrap_or_default().to_string(),
                    i["detail"].as_str().unwrap_or_default().to_string(),
                    i["sortText"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    };
    let src = concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "end\n\n",
        "attribute holds on struct as\n",
        "    requires private field state: number\n",
        "end\n\n",
        "@holds\nstruct S as\n    x: number\n    \nend\n\n",
        "@service\nimpl S as\n    \nend\n"
    );

    // The method column of the `impl`.
    let members = rows(src, "impl S as\n    ");
    assert_eq!(
        members[0],
        (
            "Start".to_string(),
            "required by `@service`".to_string(),
            "0".to_string()
        )
    );

    // The field column of the `struct`.
    let fields = rows(src, "x: number\n    ");
    assert_eq!(
        fields[0],
        (
            "state".to_string(),
            "required by `@holds`".to_string(),
            "0".to_string()
        )
    );
}

/*
The list inside `import { | }` is the module's exports and nothing else.
An attribute reads `@name` there, `@` alone narrows the list to the
module's attributes, and the local name after `as` takes no list.

The entries span lines, and a name on the second line answers the same
way: the statement, not the line, says what the position is.
*/
#[test]
pub(crate) fn an_import_list_offers_the_module_and_marks_its_attributes() {
    let dir = std::env::temp_dir().join(format!("alloy-import-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");
    std::fs::write(
        dir.join("src/lib.aly"),
        "--- Marks a struct.\nexport attribute tagged(name: string) on struct\n\n--- A count.\nexport const version = 1\n\n--- An id.\nexport type Id = number\n",
    )
    .expect("module");

    let src = "import { a, @b, c as d } from \"./lib\"\nimport {\n    e\n} from \"./lib\"\nimport type { f } from \"./lib\"\n";
    let main = dir.join("src/main.aly");
    std::fs::write(&main, src).expect("main");

    let uri = format!("file://{}", main.display());
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        ..State::default()
    };
    let options = EmitOptions {
        file_name: main.to_string_lossy().into_owned(),
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
    // The caret replaces the letter the fixture wrote, so each position
    // reads as a half-typed name.
    let labels = |mark: &str| -> Vec<String> {
        let at = src.find(mark).expect(mark) + mark.len();
        let ctx = context::detect(src, at).expect("a context");

        st.context_items(&uri, at, &ctx)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    };

    // A plain entry: every export, the attribute by its bare name.
    let names = labels("import { a");
    assert_eq!(names, ["type", "tagged", "version", "type Id"]);

    // `@` narrows the list to the attributes, and nothing else.
    assert_eq!(labels("@b"), ["tagged"]);

    // The local name after `as` is the reader's own.
    assert!(labels("c as d").is_empty());

    // The second line of a list that spans lines.
    assert_eq!(labels("    e"), ["type", "tagged", "version", "type Id"]);

    // A type-only list holds the types; an attribute is a value.
    assert_eq!(labels("import type { f"), ["Id"]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `local w = Shapes.` answered the whole global scope: the child had no
/// table for the name, so it fell back to the scope of an expression and
/// put 450 globals under a `.`. The walk over the namespaces and the
/// imports says what the path holds, and that list stands alone.
#[test]
pub(crate) fn a_dotted_value_path_never_falls_through_to_the_scope() {
    const SRC: &str = concat!(
        "import * as Star from \"./scribe\"\n",
        "namespace Shapes as\n",
        "    public function go() end\n",
        "end\n",
        "namespace Types as\n",
        "    type Box = { w: number }\n",
        "end\n",
        "local t = { a = 1 }\n",
        "local w = Shapes.\n",
        "local x = Star.\n",
        "local y = Types.\n",
        "local z = t.\n",
    );
    let st = files(&[
        ("file:///t.aly", SRC),
        (
            "file:///scribe.aly",
            "export type Store = { id: number }\nexport function open(n: string) end\n",
        ),
    ]);
    let uri = "file:///t.aly";
    // What the child answers when it cannot type the receiver: the
    // scope of an expression, keywords and globals together.
    let scope = || {
        json!([
            { "label": "nil", "kind": 6 },
            { "label": "print", "kind": 3 },
            { "label": "version", "kind": 3 },
        ])
    };
    let labels = |items: &[Value]| -> Vec<String> {
        items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let line =
        |text: &str| -> u32 { SRC[..SRC.find(text).expect(text)].matches('\n').count() as u32 };
    let column = |text: &str| text.chars().count() as u32;

    // `local w = Shapes.`: the namespace holds one function.
    let found = st
        .value_path_members(
            uri,
            line("local w ="),
            column("local w = Shapes."),
            &scope(),
        )
        .expect("the namespace answers");
    assert_eq!(labels(&found), ["go"]);
    assert_eq!(found[0]["detail"], json!("function Shapes.go"));

    // `local x = Star.`: the module the import binds.
    let found = st
        .value_path_members(uri, line("local x ="), column("local x = Star."), &scope())
        .expect("the module answers");
    assert_eq!(labels(&found), ["open"]);

    // A namespace of types alone holds no value. Nothing is the answer;
    // the scope of the file is not.
    let found = st
        .value_path_members(uri, line("local y ="), column("local y = Types."), &scope())
        .expect("the namespace answers");
    assert!(found.is_empty(), "{found:?}");

    // A plain table is the child's to answer: it types the local and
    // this list would stand in for an answer the child gives.
    assert!(
        st.value_path_members(uri, line("local z ="), column("local z = t."), &scope())
            .is_none()
    );

    // The child answered with members, so its own list stands: the
    // types come from the solver, which reads more than the source.
    let members = json!([{ "label": "go", "kind": 3, "detail": "() -> ()" }]);
    assert!(
        st.value_path_members(
            uri,
            line("local w ="),
            column("local w = Shapes."),
            &members
        )
        .is_none()
    );

    // No emit-only name reaches a label.
    for name in ["Shapes_go", "_m1", "__alloy"] {
        let found = st
            .value_path_members(
                uri,
                line("local w ="),
                column("local w = Shapes."),
                &scope(),
            )
            .expect("the namespace answers");

        assert!(!labels(&found).contains(&name.to_string()), "{name}");
    }
}

/// A flat `Enum*` name comes from the definitions file, where it names
/// the item type of one engine enum. luau-lsp keeps it out of the type
/// scope, so `local a: EnumUserInputType` does not compile.
/// `Enum.UserInputType` is the spelling a type slot takes, and the
/// dotted list is where those names belong.
#[test]
pub(crate) fn a_type_slot_takes_the_dotted_spelling_of_an_engine_enum() {
    const SRC: &str = "local a: EnumUs\nlocal b: Enum.\n";
    let (st, uri) = one_file(SRC);
    let bare = type_slot_labels(&st, uri, SRC, "local a: ");

    for flat in ["EnumUserInputType", "EnumKeyCode", "EnumKeyCode_INTERNAL"] {
        assert!(!bare.contains(&flat.to_string()), "`{flat}` is offered");
    }

    // The three datatypes that carry the prefix and are types of their
    // own stay in the list.
    for name in ["Enum", "EnumItem", "Enums"] {
        assert!(bare.contains(&name.to_string()), "`{name}` is missing");
    }

    let dotted = type_slot_labels(&st, uri, SRC, "local b: Enum.");
    assert!(dotted.contains(&"UserInputType".to_string()));
    assert!(dotted.contains(&"KeyCode".to_string()));
    // The dotted list holds the enums and nothing else: no primitive,
    // no std type, no class.
    for other in [
        "string",
        "HashMap",
        "Part",
        "Enum",
        "UserInputType_INTERNAL",
    ] {
        assert!(!dotted.contains(&other.to_string()), "`{other}` is offered");
    }
}

/// Luau's type functions reached no type slot: `export type K = ke`
/// offered every name of the workspace and no `keyof`. Each one inserts
/// its brackets and leaves the caret inside them.
#[test]
pub(crate) fn a_type_slot_offers_the_luau_type_functions() {
    const SRC: &str = concat!(
        "local a: ty\n",
        "export type K = ke\n",
        "struct S as\n",
        "    x: ind\n",
        "end\n",
    );
    let (st, uri) = one_file(SRC);

    for at in ["local a: ", "export type K = ", "    x: "] {
        let labels = type_slot_labels(&st, uri, SRC, at);

        for name in [
            "typeof",
            "keyof",
            "rawkeyof",
            "index",
            "rawget",
            "setmetatable",
            "getmetatable",
        ] {
            assert!(labels.contains(&name.to_string()), "`{name}` at `{at}`");
        }

        // `union` and `intersect` are no type functions: the checker
        // reports `Unknown type 'union'`.
        for name in ["union", "intersect"] {
            assert!(!labels.contains(&name.to_string()), "`{name}` at `{at}`");
        }
    }

    let offset = SRC.find("local a: ").expect("the slot") + "local a: ".len();
    let ctx = context::detect(SRC, offset).expect("a type slot");
    let insert = |st: &State, label: &str| -> String {
        st.context_items(uri, offset, &ctx)
            .iter()
            .find(|i| i["label"] == json!(label))
            .and_then(|i| i["textEdit"]["newText"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    // The accept writes the brackets and puts the caret between them.
    assert_eq!(insert(&st, "typeof"), "typeof($1)");
    assert_eq!(insert(&st, "keyof"), "keyof<$1>");

    // With no snippet support the placeholder would land as literal
    // text, so the pair goes in empty.
    let mut plain = st;
    plain.snippets = false;
    assert_eq!(insert(&plain, "typeof"), "typeof()");
    assert_eq!(insert(&plain, "keyof"), "keyof<>");
}

/// The body of an `attribute ... as` offered the whole global scope
/// after `requires`, and nothing at all after the kind word. The list is
/// the words of a contract clause, and the scope reaches no position of
/// the body.
#[test]
pub(crate) fn an_attribute_contract_lists_its_own_words() {
    const SRC: &str = concat!(
        "attribute p(items: string[], name: string) on impl as\n",
        "    requires \n",
        "    requires private function \n",
        "    requires private function each \n",
        "    \n",
        "end\n",
    );
    let (st, uri) = one_file(SRC);
    let labels = |at: &str| -> Vec<String> {
        let offset = SRC.find(at).expect(at) + at.len();
        let ctx = context::detect(SRC, offset).expect("a context");

        st.context_items(uri, offset, &ctx)
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect()
    };

    assert_eq!(
        labels("    requires "),
        ["public", "private", "function", "field"]
    );
    assert_eq!(labels("    requires private function "), ["each"]);
    assert_eq!(labels("    requires private function each "), ["items"]);
    assert_eq!(labels("\n    \n"), ["requires", "end"]);

    // No position of the body reaches the scope. `print` and `game` are
    // the two the whole global list opened with.
    for at in [
        "    requires ",
        "    requires private function ",
        "    requires private function each ",
        "\n    \n",
    ] {
        let held = labels(at);

        for name in ["print", "game", "string", "HashMap"] {
            assert!(!held.contains(&name.to_string()), "`{name}` at `{at}`");
        }
    }

    // `requires private function |` answered with nothing at all: the
    // word before the caret is `function`, which the name rule read as a
    // declaration of its own.
    let offset = SRC
        .find("    requires private function ")
        .expect("the slot")
        + "    requires private function ".len();
    assert!(!declares_a_name_at(SRC, offset));
    // A `function` that does declare a name still does.
    assert!(declares_a_name_at("local function |", 15));
}

/// `Unknown type 'Geo.Vec'`: a member of a namespace is reached through
/// its group, so the fix imports `Geo`.
#[test]
fn an_unresolved_namespace_member_offers_its_group() {
    let st = super::support::files(&[
        (
            "file:///defs.aly",
            "export namespace Geo as\n    public struct Vec as\n        x: number\n    end\nend\n",
        ),
        (
            "file:///use.aly",
            "local function g(): Geo.Vec\n    return new Geo.Vec { x = 1 }\nend\n",
        ),
    ]);
    let report = json!({
        "message": "TypeError: Unknown type 'Geo.Vec'",
        "range": { "start": { "line": 0, "character": 20 }, "end": { "line": 0, "character": 27 } },
    });
    let actions = st.import_actions("file:///use.aly", &[report]);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(
        actions[0]["edit"]["changes"]["file:///use.aly"][0]["newText"],
        json!("import { Geo } from \"./defs\"\n")
    );
}

/// An enum a file reads as a type and as a value: the report on the
/// annotation offers the value form, which serves the type too, so one
/// fix resolves the file.
#[test]
fn a_name_read_as_a_member_takes_the_value_import() {
    let st = super::support::files(&[
        (
            "file:///defs.aly",
            "export enum Status as\n    Ok,\n    Bad(string)\nend\n",
        ),
        (
            "file:///use.aly",
            "local function st(): Status\n    return Status.Ok\nend\n",
        ),
    ]);
    let report = json!({
        "message": "TypeError: Unknown type 'Status'",
        "range": { "start": { "line": 0, "character": 21 }, "end": { "line": 0, "character": 27 } },
    });
    let actions = st.import_actions("file:///use.aly", &[report]);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(
        actions[0]["edit"]["changes"]["file:///use.aly"][0]["newText"],
        json!("import { Status } from \"./defs\"\n")
    );
}

/// `Unknown type 'Vec2'` where another module exports `Vec2`: the
/// quick fix writes the import line the completion list would insert.
#[test]
pub(crate) fn an_unresolved_name_offers_the_import_that_binds_it() {
    let st = super::support::files(&[
        (
            "file:///defs.aly",
            "export struct Vec2 as\n    x: number\nend\n",
        ),
        (
            "file:///use.aly",
            "local function make(): Vec2\n    return new Vec2 { x = 1 }\nend\n",
        ),
    ]);
    let report = json!({
        "message": "TypeError: Unknown type 'Vec2'",
        "range": { "start": { "line": 0, "character": 23 }, "end": { "line": 0, "character": 27 } },
    });
    let actions = st.import_actions("file:///use.aly", &[report]);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(
        actions[0]["title"],
        json!("Add `import { Vec2 } from \"./defs\"`")
    );
    assert_eq!(actions[0]["kind"], json!("quickfix"));
    assert_eq!(
        actions[0]["edit"]["changes"]["file:///use.aly"][0]["newText"],
        json!("import { Vec2 } from \"./defs\"\n")
    );

    // `new Vec2 { }` reports a type with no struct behind it; the same
    // import is the fix.
    let built = json!({ "message": "TypeError: `Vec2` is a type, not a struct" });
    let actions = st.import_actions("file:///use.aly", &[built]);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(
        actions[0]["title"],
        json!("Add `import { Vec2 } from \"./defs\"`")
    );

    // A report that names nothing to import offers nothing.
    let other = json!({ "message": "unused_variable: `x` is never read" });
    assert!(
        st.import_actions("file:///use.aly", &[other]).is_empty(),
        "a report with no unresolved name"
    );
}

/// `Unknown type 'Point'` on an annotation: the quick fix imports the
/// name as a type, joins an `import { ... }` line the file already has
/// for the module, and keeps the value form where the file also writes
/// `new Point`.
#[test]
pub(crate) fn an_unresolved_type_name_offers_the_type_import() {
    let point = "export struct Point as\n    x: number\nend\n\nexport type Pair = { a: number }\n\nexport function other(): number\n    return 1\nend\n";
    let fix = |src: &str, message: &str| -> Vec<(String, Value)> {
        let st = super::support::files(&[("file:///point.aly", point), ("file:///use.aly", src)]);
        let report = json!({ "message": message });

        st.import_actions("file:///use.aly", &[report])
            .iter()
            .map(|a| {
                (
                    a["title"].as_str().unwrap_or_default().to_string(),
                    a["edit"]["changes"]["file:///use.aly"][0].clone(),
                )
            })
            .collect()
    };
    let unknown = "TypeError: Unknown type 'Point'";
    let fresh = json!({
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        "newText": "import { type Point } from \"./point\"\n",
    });

    // A struct an annotation alone names imports as a type, on a fresh
    // line.
    assert_eq!(
        fix("local p: Point = nil\nprint(p)\n", unknown),
        [(
            "Add `import { type Point } from \"./point\"`".to_string(),
            fresh.clone()
        )]
    );
    // The file also constructs it: the value form binds the type too.
    assert_eq!(
        fix("local p: Point = new Point { x = 1 }\nprint(p)\n", unknown)[0].0,
        "Add `import { Point } from \"./point\"`"
    );
    // A `new PointList` is another name.
    assert_eq!(
        fix(
            "local p: Point = new PointList { x = 1 }\nprint(p)\n",
            unknown
        )[0]
        .0,
        "Add `import { type Point } from \"./point\"`"
    );
    // The module is imported already: the name joins that line.
    let src = "import { other } from \"./point\"\n\nlocal p: Point = nil\nprint(p, other)\n";
    assert_eq!(
        fix(src, unknown),
        [(
            "Add `import { type Point } from \"./point\"`".to_string(),
            json!({
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 31 } },
                "newText": "import { other, type Point } from \"./point\"",
            })
        )]
    );
    // A type alias joins the same way.
    assert_eq!(
        fix(src, "TypeError: Unknown type 'Pair'")[0].1["newText"],
        json!("import { other, type Pair } from \"./point\"")
    );
}

/// `self.` inside an `impl` of an imported struct listed the fields
/// alone: the emit writes the methods on the module's table, and the
/// checker types that table from the module.
#[test]
fn an_impl_of_an_imported_struct_lists_its_own_methods() {
    let point = "export struct Point as\n    x: number\n    y: number\nend\n";
    let block = concat!(
        "import { Point } from \"./point\"\n",
        "\n",
        "export impl Point as\n",
        "    function length(self): number\n",
        "        return self.x\n",
        "    end\n",
        "\n",
        "    private function scaled(self, factor: number): Point\n",
        "        return self\n",
        "    end\n",
        "\n",
        "    function of(x: number): Point\n",
        "        return new Point { x = x, y = 0 }\n",
        "    end\n",
        "end\n",
    );
    let st = files(&[
        ("file:///point.aly", point),
        ("file:///point_impl.aly", block),
    ]);
    let fields = json!([{ "label": "x" }, { "label": "y" }]);
    let items = st.impl_self_members("file:///point_impl.aly", 4, 20, &fields);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    // A private method is the impl's own, and it sits in this file.
    // `of` takes no receiver, so no `self.` reaches it.
    assert_eq!(labels, ["length", "scaled"], "{items:?}");
    assert_eq!(items[0]["detail"], json!("(self) -> number"));

    // A field the child already listed comes once.
    assert!(
        st.impl_self_members(
            "file:///point_impl.aly",
            4,
            20,
            &json!([{ "label": "length" }])
        )
        .iter()
        .all(|i| i["label"] != json!("length"))
    );

    // Outside an impl nothing is added.
    assert!(
        st.impl_self_members("file:///point.aly", 1, 6, &fields)
            .is_empty()
    );
}

/// The child offers a static after a `self.`: the emit writes a method
/// with no receiver on the same table as the methods that take one.
#[test]
fn a_static_never_follows_self() {
    let block = concat!(
        "struct Widget as\n",
        "    w: number\n",
        "end\n",
        "\n",
        "impl Widget as\n",
        "    function zero(): Widget\n",
        "        return new Widget { w = 0 }\n",
        "    end\n",
        "\n",
        "    function inst(self): number\n",
        "        return self.w\n",
        "    end\n",
        "end\n",
    );
    let st = files(&[("file:///widget.aly", block)]);
    let mut result = json!([{ "label": "zero" }, { "label": "inst" }, { "label": "w" }]);
    st.drop_impl_statics("file:///widget.aly", 10, 20, &mut result);

    assert_eq!(result, json!([{ "label": "inst" }, { "label": "w" }]));

    // `Widget.zero()` still reaches it: the filter reads the `self.`.
    let mut whole = json!([{ "label": "zero" }]);
    st.drop_impl_statics("file:///widget.aly", 5, 12, &mut whole);

    assert_eq!(whole, json!([{ "label": "zero" }]));
}

/// `import { logit as log }`: a macro's declaration carries its sigil and
/// the name the module wrote, `$logit`, so the alias reached it under no
/// name. The hover was empty and the `$` list left it out.
#[test]
fn an_aliased_macro_reads_under_the_name_the_file_writes() {
    let use_src =
        "import { logit as log } from \"./m\"\nimport { tag as tb } from \"./b\"\n\n$log(\"hi\")\n";
    let st = files(&[
        (
            "file:///m.aly",
            "export macro logit(x)\n    print(x)\nend\n",
        ),
        (
            "file:///b.aly",
            "export macro tag(x)\n    print(\"B\", x)\nend\n",
        ),
        ("file:///use.aly", use_src),
    ]);
    let found = st.aliased_macros("file:///use.aly");
    let names: Vec<(&str, &str)> = found
        .iter()
        .map(|(bound, d)| (bound.as_str(), d.name.as_str()))
        .collect();

    assert_eq!(names, vec![("log", "$logit"), ("tb", "$tag")]);
    assert!(found[1].1.hover.contains("B"), "{}", found[1].1.hover);

    // The hover of `$log` reads the export's sigil name the same way.
    assert_eq!(
        import_alias_source(use_src, "log"),
        Some("logit".to_string())
    );
}

/// Signature help the proxy answers itself. A macro call is gone from
/// the emit, and a file with an unclosed `(` has no compile at all, so
/// the shadow stays the Alloy source and the child parses none of it.
/// That file is the one a reader asking for a signature always has.
#[test]
fn a_macro_and_an_unclosed_call_answer_from_the_declaration() {
    const SRC: &str = concat!(
        "macro double(x) x * 2 end\n",
        "\n",
        "export function generic_id<T>(v: T, extra: number): T\n",
        "    return v\n",
        "end\n",
        "\n",
        "print(generic_id(1,\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let label = |help: &Value| {
        help["signatures"][0]["label"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let params = |help: &Value| {
        help["signatures"][0]["parameters"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|p| p["label"].as_str().unwrap_or("").to_string())
            .collect::<Vec<String>>()
    };

    // Past the comma: the second parameter is the one being written.
    let help = st.declared_signature_help(uri, 6, 19).expect("the call");
    assert_eq!(
        label(&help),
        "function generic_id<T>(v: T, extra: number): T"
    );
    assert_eq!(params(&help), ["v: T", "extra: number"]);
    assert_eq!(help["activeParameter"], json!(1));

    // A macro reads by its sigil, and its body is no part of the
    // signature.
    let (st2, uri2) = super::support::one_file("macro double(x) x * 2 end\n\nprint($double(\n");
    let help = st2.declared_signature_help(uri2, 2, 14).expect("the call");
    assert_eq!(label(&help), "macro double(x)");
    assert_eq!(params(&help), ["x"]);
    assert_eq!(help["activeParameter"], json!(0));

    // A member of a value stays with the child, which types the
    // receiver; the proxy reads no declaration for one.
    let (st3, uri3) = super::support::one_file("local w = 1\nprint(w:combine(\n");
    assert!(st3.declared_signature_help(uri3, 1, 16).is_none());
}

/// A struct inside a namespace answers by its path, at one level and
/// at three. A `*` import puts the module's own name in front of the
/// path, and the fields the list offers are still the struct's.
#[test]
fn a_namespaced_struct_literal_lists_its_fields() {
    let dir = super::documents::alias_root(
        "namespace-literal",
        &[(
            "src/lib.aly",
            "export namespace Ns as\n    struct T as\n        n: number\n    end\nend\n",
        )],
    );
    let src = concat!(
        "import * as M from \"./lib\"\n",
        "\n",
        "namespace One as\n",
        "    struct Pair as\n",
        "        left: number\n",
        "    end\n",
        "end\n",
        "\n",
        "namespace Outer as\n",
        "    namespace Inner as\n",
        "        namespace Deep as\n",
        "            struct Deep as\n",
        "                q: number\n",
        "            end\n",
        "        end\n",
        "    end\n",
        "end\n",
        "\n",
        "local a = new One.Pair { \n",
        "local b = new Outer.Inner.Deep.Deep { \n",
        "local c = new M.Ns.T { \n",
    );
    let uri = path_to_uri(&dir.join("src/main.aly"));
    let st = files(&[(uri.as_str(), src)]);
    let labels = |head: &str| -> Vec<String> {
        let at = src.find(head).expect("the literal") + head.len();
        let ctx = context::detect(src, at).expect("a context");

        st.context_items(&uri, at, &ctx)
            .iter()
            .map(|i| i["label"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    let one = labels("new One.Pair { ");
    let three = labels("new Outer.Inner.Deep.Deep { ");
    let star = labels("new M.Ns.T { ");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(one, ["left"]);
    assert_eq!(three, ["q"]);
    assert_eq!(star, ["n"]);
}

/// `import { Ns as A }` binds the group under another name; the
/// literal `new A.T { ` still lists the fields the module declares.
#[test]
fn an_aliased_namespace_literal_lists_its_fields() {
    let dir = super::documents::alias_root(
        "namespace-alias-literal",
        &[(
            "src/lib.aly",
            "export namespace Ns as\n    struct T as\n        n: number\n    end\nend\n",
        )],
    );
    let src = "import { Ns as A } from \"./lib\"\n\nlocal c = new A.T { \n";
    let uri = path_to_uri(&dir.join("src/main.aly"));
    let st = files(&[(uri.as_str(), src)]);
    let at = src.len() - 1;
    let ctx = context::detect(src, at).expect("a context");
    let labels: Vec<String> = st
        .context_items(&uri, at, &ctx)
        .iter()
        .map(|i| i["label"].as_str().unwrap_or_default().to_string())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(labels, ["n"]);
}

/// A top-level function is callable above its declaration, and the
/// emit binds it as a `local function` the child scopes from that line
/// down. The list above the line gets the function from the source,
/// with its header as the detail; below the line the child has it.
#[test]
fn a_function_declared_below_the_caret_is_offered_there() {
    let src = concat!(
        "function caller()\n",
        "    hel\n",
        "end\n",
        "\n",
        "--- Prints a line.\n",
        "function helperBelow(n: number): string\n",
        "    print(\"i am below\")\n",
        "end\n",
        "\n",
        "hel\n",
    );
    let (st, uri) = one_file(src);
    let items = st.functions_below(uri, 1, 7, &json!([]));

    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["label"], "helperBelow");
    assert_eq!(items[0]["kind"], 3);
    assert_eq!(
        items[0]["detail"],
        "function helperBelow(n: number): string"
    );
    assert_eq!(items[0]["documentation"]["value"], "Prints a line.");

    // The child lists it already, and below the declaration the child
    // is the one that has it.
    assert!(
        st.functions_below(uri, 1, 7, &json!([{ "label": "helperBelow" }]))
            .is_empty()
    );
    assert!(st.functions_below(uri, 9, 3, &json!([])).is_empty());
}

/// `$assert_eq(p:|, 5)`: the intrinsic quotes the argument for its
/// message, so `p:` stands twice on the lowered line. The member list
/// belongs after the code, not inside the string.
#[test]
pub(crate) fn a_member_in_a_macro_argument_finds_the_code() {
    let head = concat!(
        "struct Point as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "local p = new Point { x = 3 }\n",
    );

    for line in ["$assert_eq(p:len(), 3)", "$dbg(p:len())"] {
        let src = format!("{head}{line}\n");
        let (st, uri) = one_file(&src);
        let doc = st.docs.get(uri).unwrap();
        let column = line.find("p:").unwrap() + 2;
        let (shadow_no, _) = doc.to_shadow(5, column as u32);
        let shadow_line = doc.shadow.lines().nth(shadow_no as usize).unwrap();
        let at = context::member_column(
            line,
            shadow_line,
            "p",
            context::Access::Plain,
            ':',
            0,
            column,
        )
        .unwrap_or_else(|| panic!("{line}: {shadow_line}"));

        assert!(shadow_line[..at].ends_with("p:"), "{line}: {shadow_line}");
        assert!(
            !context::in_string(shadow_line, at),
            "{line}: {shadow_line}"
        );
    }
}

/// Signature help inside an intrinsic's arguments: the expansion is
/// generated text, so the child answers nothing, and no source
/// declares the intrinsic. The documentation writes its call shape.
#[test]
fn an_intrinsic_call_answers_signature_help_from_its_documentation() {
    const SRC: &str = concat!(
        "local p = { x = 1 }\n",
        "local dbg_val = $dbg(p.x)\n",
        "local str_val = $stringify(p.x)\n",
        "$assert_eq(p.x, 1)\n",
        "print(1)\n",
    );
    let (st, uri) = one_file(SRC);
    let label = |help: &Value| help["signatures"][0]["label"].as_str().unwrap().to_string();

    let help = st.declared_signature_help(uri, 1, 21).expect("$dbg");
    assert_eq!(label(&help), "$dbg(expr)");
    assert_eq!(
        help["signatures"][0]["parameters"][0]["label"],
        json!("expr")
    );
    assert_eq!(help["activeParameter"], json!(0));

    let help = st.declared_signature_help(uri, 2, 27).expect("$stringify");
    assert_eq!(label(&help), "$stringify(expr)");

    // Past the comma: the second parameter is the one being written.
    let help = st.declared_signature_help(uri, 3, 16).expect("$assert_eq");
    assert_eq!(label(&help), "$assert_eq(a, b)");
    assert_eq!(help["activeParameter"], json!(1));

    // A plain call is still the child's.
    assert!(st.declared_signature_help(uri, 4, 6).is_none());
}

/// A caret inside the string argument of an intrinsic: the expansion
/// holds no byte of the string at that position, so the child would
/// list the whole scope. A string the emit copies keeps the child's
/// own answer.
#[test]
fn a_string_an_intrinsic_rewrote_lists_nothing() {
    use super::super::completion::string_left_behind;

    const SRC: &str = concat!(
        "$todo(\"handle error case\")\n",
        "print(\"hello world\")\n",
        "local s = game:GetService(\"Players\")\n",
    );
    let (st, uri) = one_file(SRC);
    let doc = st.docs.get(uri).unwrap();
    let column = |line: &str, text: &str| (line.find(text).unwrap() + text.len()) as u32;

    assert!(string_left_behind(
        doc,
        0,
        column("$todo(\"handle error case\")", "handle")
    ));
    assert!(!string_left_behind(
        doc,
        1,
        column("print(\"hello world\")", "hello")
    ));
    assert!(!string_left_behind(
        doc,
        2,
        column("local s = game:GetService(\"Players\")", "Pl")
    ));
    // Outside every string the child answers as before.
    assert!(!string_left_behind(doc, 1, 3));
}
