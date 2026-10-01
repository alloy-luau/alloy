//! The server on Parallel Luau: `parallel do`, `message`, and the
//! `.actor.` infix.

use super::super::*;
use super::support::{files, one_file};

fn labels(st: &State, uri: &str, src: &str, head: &str) -> Vec<String> {
    let at = src.rfind(head).expect("the head") + head.len();
    let ctx = context::detect(src, at).expect("a context");

    st.context_items(uri, at, &ctx)
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect()
}

/// `parallel ` opens its block with `do`, and a message header takes
/// `as parallel` and then the word.
#[test]
fn the_heads_offer_the_word_that_follows() {
    let src = "local function f()\n    parallel \nend\n";
    let (st, uri) = one_file(src);
    assert_eq!(labels(&st, uri, src, "parallel "), ["do"]);

    let src = "message Ping(count: number) \n";
    let (st, uri) = one_file(src);
    assert_eq!(labels(&st, uri, src, "number) "), ["reply", "as parallel"]);

    // After the reply, the phase is all that is left.
    let src = "message Ping(count: number) reply(ok: boolean) \n";
    let (st, uri) = one_file(src);
    assert_eq!(labels(&st, uri, src, "boolean) "), ["as parallel"]);

    let src = "export message Ping() as \n";
    let (st, uri) = one_file(src);
    assert_eq!(labels(&st, uri, src, "as "), ["parallel"]);
}

/// Inside `Ping.on(function(`, the help lists the message's parameters
/// with the one the caret is in, for a message of this file and for an
/// imported one.
#[test]
fn a_handler_lists_the_message_parameters() {
    let src = "message Ping(count: number, cb: (number) -> (), label: string)\nPing.on(function(count, \n";
    let (st, uri) = one_file(src);
    let help = st
        .message_handler_signature(uri, 1, 24)
        .expect("the handler help");

    assert_eq!(
        help["signatures"][0]["label"],
        json!("function(count: number, cb: (number) -> (), label: string)")
    );
    assert_eq!(help["activeParameter"], json!(1));
    assert!(st.message_handler_signature(uri, 1, 7).is_none());

    let st = files(&[
        ("file:///net.aly", "export message Step(dt: number)\n"),
        (
            "file:///w.server.actor.aly",
            "import { Step } from \"./net\"\nStep.once(function(\n",
        ),
    ]);
    let mut st = st;
    let text = st.docs["file:///net.aly"].source.clone();
    st.docs
        .get_mut("file:///w.server.actor.aly")
        .expect("the worker")
        .import_sources = vec![text];
    let help = st
        .message_handler_signature("file:///w.server.actor.aly", 1, 19)
        .expect("the imported handler");

    assert_eq!(
        help["signatures"][0]["label"],
        json!("function(dt: number)")
    );
    assert_eq!(help["activeParameter"], json!(0));
}

/// A message hovers as its declaration with the comment above it, the
/// way a remote does.
#[test]
fn a_message_hovers_as_its_declaration() {
    let src = "--- One tick.\nexport message Step(dt: number) as parallel\n";

    assert_eq!(
        super::super::hover::remote_hover(src, "Step").as_deref(),
        Some("```alloy\nexport message Step(dt: number) as parallel\n```\n\nOne tick.")
    );
}

/// `parallel` in a block and in `as parallel` hovers as the phase it
/// names.
#[test]
fn parallel_hovers_as_its_phase() {
    let src = "message Hit() as parallel\nparallel do\nend\nlocal parallel = 1\n";
    let block = crate::keywords::hover(src, src.find("parallel do").unwrap())
        .expect("the block")
        .2;
    let handler = crate::keywords::hover(src, src.find("parallel\n").unwrap())
        .expect("the handler")
        .2;

    assert!(
        block.contains("Runs the block in the parallel phase"),
        "{block}"
    );
    assert!(
        handler.contains("The handler runs in the parallel phase"),
        "{handler}"
    );
    assert!(crate::keywords::hover(src, src.rfind("parallel").unwrap()).is_none());
}

/// `@` above a message offers the attributes that take the target.
#[test]
fn an_attribute_above_a_message_reads_its_target() {
    let src = "@\nmessage Ping()\n";

    match context::detect(src, 1) {
        Some(context::Context::Attribute { target, .. }) => assert_eq!(target, Some("message")),

        other => panic!("{other:?}"),
    }

    assert!(crate::names::builtin_attribute_targets("@allow").contains(&"message"));
    assert!(!crate::names::builtin_attribute_targets("@deprecated").contains(&"message"));
}

/// A refused write in a `parallel` block moves after the block.
#[test]
fn a_refused_write_moves_after_the_block() {
    let src = "local function step(part: BasePart)\n    parallel do\n        part.Position = Vector3.zero\n        print(part.Name)\n    end\nend\n";
    let (st, uri) = one_file(src);
    let actions = st.compiler_actions(uri, ((0, 0), (6, 0)));
    let action = actions
        .iter()
        .find(|a| a["title"] == "Move the write after the block")
        .expect("the move");

    assert_eq!(
        action["edit"]["changes"][uri],
        json!([
            {
                "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 0 } },
                "newText": "    part.Position = Vector3.zero\n",
            },
            {
                "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 3, "character": 0 } },
                "newText": "",
            },
        ])
    );
}

/// Outside an actor script a message's list holds `fire` and `topic`:
/// `on` and `once` have no Actor to bind on there.
#[test]
fn a_plain_file_lists_no_binding() {
    let src = "message Ping()\nPing.\n";
    let items = || {
        json!([
            { "label": "topic" },
            { "label": "fire" },
            { "label": "on" },
            { "label": "once" },
        ])
    };
    let listed = |uri: &str| {
        let st = files(&[(uri, src)]);
        let mut result = items();
        st.filter_remote_members(uri, 1, 5, &mut result);

        result
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect::<Vec<_>>()
    };

    assert_eq!(listed("file:///main.server.aly"), ["topic", "fire"]);
    assert_eq!(
        listed("file:///main.server.actor.aly"),
        ["topic", "fire", "on", "once"]
    );
}

/// With a reply, the handler of `on` lists `respond` after the message's
/// parameters, typed from the reply, and `replied` lists the reply. A
/// message with no reply has no `replied` help.
#[test]
fn a_reply_shapes_the_handler_help() {
    let src = "message Light(job: number, input: buffer) reply(job: number, levels: buffer)\nmessage Ping(n: number)\nLight.on(function(job, input, \nLight.replied(function(job, \nPing.replied(function(\n";
    let (st, uri) = one_file(src);
    let on = st
        .message_handler_signature(uri, 2, 30)
        .expect("the handler help");

    assert_eq!(
        on["signatures"][0]["label"],
        json!("function(job: number, input: buffer, respond: (job: number, levels: buffer) -> ())")
    );
    assert_eq!(on["activeParameter"], json!(2));

    let replied = st
        .message_handler_signature(uri, 3, 28)
        .expect("the replied help");
    assert_eq!(
        replied["signatures"][0]["label"],
        json!("function(job: number, levels: buffer)")
    );
    assert_eq!(replied["activeParameter"], json!(1));

    assert!(st.message_handler_signature(uri, 4, 22).is_none());
}

/// The parameter being named in a handler list takes the name the
/// declaration gives it, `respond` last.
#[test]
fn a_handler_parameter_completes_its_name() {
    let src = "message Light(job: number) reply(levels: buffer)\nLight.on(function(job, \nLight.replied(function(\n";
    let (st, uri) = one_file(src);
    let on = st
        .message_handler_completion(uri, 1, 23)
        .expect("the respond item");

    assert_eq!(on[0]["label"], json!("respond"));
    assert_eq!(on[0]["detail"], json!("respond: (levels: buffer) -> ()"));

    let replied = st
        .message_handler_completion(uri, 2, 23)
        .expect("the reply item");
    assert_eq!(replied[0]["label"], json!("levels"));
}

/// Inside `respond(`, the help lists the reply of the message whose
/// handler binds the name. A `respond` out of scope answers nothing.
#[test]
fn respond_lists_the_reply() {
    let src = "message Light(job: number) reply(job: number, levels: buffer)\nLight.on(function(job, done)\n    done(job, \nend)\ndone(\n";
    let (st, uri) = one_file(src);
    let help = st.respond_signature(uri, 2, 14).expect("the respond help");

    assert_eq!(
        help["signatures"][0]["label"],
        json!("done(job: number, levels: buffer)")
    );
    assert_eq!(help["activeParameter"], json!(1));
    assert!(st.respond_signature(uri, 4, 5).is_none());
}

/// A message's list holds `replied` beside the four, and a plain file
/// keeps it: `replied` binds on any script.
#[test]
fn a_plain_file_keeps_replied() {
    let src = "message Ping() reply()\nPing.\n";
    let st = files(&[("file:///main.server.aly", src)]);
    let mut result = json!([
        { "label": "topic" },
        { "label": "fire" },
        { "label": "on" },
        { "label": "once" },
        { "label": "replied" },
    ]);
    st.filter_remote_members("file:///main.server.aly", 1, 5, &mut result);
    let labels: Vec<&str> = result
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();

    assert_eq!(labels, ["topic", "fire", "replied"]);
}

/// The `reply` of a header hovers as the clause, a message hovers with
/// its reply, and `@` above one still reads the target.
#[test]
fn a_reply_hovers_and_keeps_the_attribute_target() {
    let src =
        "--- A job.\nexport message Light(job: number) reply(levels: buffer)\nlocal reply = 1\n";
    let word = crate::keywords::hover(src, src.find("reply(").unwrap())
        .expect("the clause")
        .2;

    assert!(word.contains("The answer of a `message`"), "{word}");
    assert!(crate::keywords::hover(src, src.rfind("reply").unwrap()).is_none());
    assert_eq!(
        super::super::hover::remote_hover(src, "Light").as_deref(),
        Some("```alloy\nexport message Light(job: number) reply(levels: buffer)\n```\n\nA job.")
    );

    match context::detect("@\nmessage Ping() reply()\n", 1) {
        Some(context::Context::Attribute { target, .. }) => assert_eq!(target, Some("message")),

        other => panic!("{other:?}"),
    }

    // The list of `reply(` declares, so no call's help answers there.
    let head = "message Light(job: number) reply(";
    let start = head.rfind("reply").unwrap();
    assert!(crate::proxy::completion::declares_params(head, start));
}
