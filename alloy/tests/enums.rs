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
