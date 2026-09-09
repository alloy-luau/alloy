use super::super::*;
use super::support::one_file;

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
