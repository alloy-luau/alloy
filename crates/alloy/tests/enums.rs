//! Enum declarations, and the mistakes a Rust habit makes in one.

fn messages(src: &str) -> Vec<String> {
    alloy::compile_with(src, &alloy::EmitOptions::default())
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

/// `Playing(round: number)` is one mistake, so it reads once, at the
/// colon, and the parse goes on: the rest of the enum still compiles.
#[test]
fn a_named_payload_reads_once_and_names_the_form() {
    let src = "enum Phase as\n    Lobby\n    Playing(round: number)\nend\n\nreturn Phase\n";
    assert_eq!(
        messages(src),
        vec!["an enum payload is a type, not a name; write `Playing(number)`"]
    );

    let out = alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap();
    assert!(out.ship.contains("Playing"), "{}", out.ship);
}

/// A payload with no name still parses with nothing to say.
#[test]
fn a_plain_payload_says_nothing() {
    let src = "enum Phase as\n    Lobby\n    Playing(number)\n    Over(string, number)\nend\n\nreturn Phase\n";
    assert!(messages(src).is_empty(), "{:?}", messages(src));
}

/// A constructor writes its type arguments the way a call does. One
/// `<` reads as a comparison, so the message names the form.
#[test]
fn a_constructor_names_the_type_argument_form() {
    let src = "struct Pair<A, B> as\n    first: A\n    second: B\nend\n\nlocal p = new Pair<number, string> { first = 1, second = \"a\" }\n\nreturn p\n";
    assert_eq!(
        messages(src),
        vec!["`new` writes its type arguments in `<<...>>`, not in `<...>`"]
    );

    let good = "struct Pair<A, B> as\n    first: A\n    second: B\nend\n\nlocal p = new Pair<<number, string>> { first = 1, second = \"a\" }\n\nreturn p\n";
    assert!(messages(good).is_empty(), "{:?}", messages(good));
}

/// A lenient parse of an unclosed `enum` leaves a body whose last token
/// is the last variant, so the gap after it runs backwards. The desugar
/// read that gap and panicked on the byte range.
#[test]
fn an_unclosed_enum_reports_and_does_not_panic() {
    let src = "enum E as\n    A\n    B\n\nfunction later(): number\n    return 1\nend\n";
    let got = messages(src);
    assert!(got.iter().any(|m| m.contains("needs an `end`")), "{got:?}");
}

/// `@derive(Debug)` prints every variant under the enum's name. A unit
/// variant is a string with no metatable, and `debug` gave it bare:
/// `Quit` beside `Event.Scored(3, "bob")`.
#[test]
fn a_derived_debug_names_the_enum_for_every_variant() {
    let src = concat!(
        "@derive(Debug)\n",
        "enum Event\n",
        "    Scored(number, string)\n",
        "    Quit\n",
        "end\n",
        "local seen = `{Event.debug(Event.Quit)}|{Event.Scored(3, \"bob\"):debug()}`\n",
        "export const project = { name = seen }\n",
    );
    let config = alloy::config_aly::evaluate_source(src, std::path::Path::new(".config.aly"))
        .expect("the code runs");

    assert_eq!(
        config["project"]["name"].as_str(),
        Some("Event.Quit|Event.Scored(3, \"bob\")")
    );
}

/// A unit variant in a payload slot or a field printed as a bare
/// string, `Item.Tool("Axe", 1)`, and a field that held a plain table
/// printed its address. The printer knows each declared type now.
#[test]
fn a_printed_unit_variant_names_its_enum_in_a_slot_and_a_field() {
    let src = concat!(
        "enum Kind\n    Axe\nend\n",
        "@derive(Debug)\nenum Item\n    Tool(Kind, number)\n    Junk\nend\n",
        "struct Stack\n    item: Item\nend\n",
        "@derive(Debug)\nstruct Bag\n    slots: { Stack }\nend\n",
        "local tool = Item.Tool(Kind.Axe, 1):debug()\n",
        "local stack = tostring(new Stack { item = Item.Junk })\n",
        "local bag = new Bag { slots = { new Stack { item = Item.Junk } } }:debug()\n",
        "export const project = { name = `{tool}|{stack}|{bag}` }\n",
    );
    let config = alloy::config_aly::evaluate_source(src, std::path::Path::new(".config.aly"))
        .expect("the code runs");

    assert_eq!(
        config["project"]["name"].as_str(),
        Some(
            "Item.Tool(Kind.Axe, 1)|Stack { item = Item.Junk }|Bag { slots = [ Stack { item = Item.Junk } ] }"
        )
    );
}
