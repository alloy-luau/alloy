//! Attribute contracts: the `requires` clauses of an
//! `attribute ... as ... end`, checked where the attribute is used.

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

fn messages(src: &str) -> Vec<String> {
    compile(src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

/// The one message a source reports, with no other.
#[track_caller]
fn one(src: &str) -> String {
    let m = messages(src);
    assert_eq!(m.len(), 1, "{m:?}");

    m.into_iter().next().expect("a message")
}

#[track_caller]
fn clean(src: &str) {
    let out = compile(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}

const LIFECYCLE: &str = "enum Lifecycle as\n    Init\n    Start\nend\n\n";

/// Every clause form parses and holds.
#[test]
fn a_met_contract_reports_nothing() {
    clean(concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "    requires private field state: number\n",
        "    requires function Stop(self): boolean\n",
        "end\n\n",
        "struct S as\n    private state: number\nend\n\n",
        "@service\nimpl S as\n",
        "    public function Start(self): ()\n    end\n\n",
        "    function Stop(self): boolean\n        return true\n    end\nend\n\n",
        "print(S)\n"
    ));
}

/// A clause with no visibility takes either one.
#[test]
fn an_open_clause_takes_both_visibilities() {
    let body = |visibility: &str| {
        format!(
            "attribute service on impl as\n    requires function Start(self)\nend\n\nstruct S as\n    x: number\nend\n\n@service\nimpl S as\n    {visibility} function Start(self): ()\n    end\nend\n\nprint(S)\n"
        )
    };

    clean(&body("private"));
    clean(&body("public"));
}

/// The three reports of a use, in the words the RFC wrote.
#[test]
fn a_missing_member_names_the_member_and_the_owner() {
    let m = one(concat!(
        "attribute provider on impl as\n",
        "    requires private function Init(self)\n",
        "end\n\n",
        "struct PlayerData as\n    x: number\nend\n\n",
        "@provider\nimpl PlayerData as\nend\n\nprint(PlayerData)\n"
    ));
    assert_eq!(
        m,
        "`@provider` requires a private function `Init(self)`; `PlayerData` declares none"
    );
    assert_eq!(alloy::docs::kind_for(&m), "AttributeContract");
    assert_eq!(alloy::docs::code_for(&m), Some("3.11"));
}

#[test]
fn a_wrong_visibility_names_both_sides() {
    let m = one(concat!(
        "attribute provider on impl as\n",
        "    requires private function Init(self)\n",
        "end\n\n",
        "struct PlayerData as\n    x: number\nend\n\n",
        "@provider\nimpl PlayerData as\n",
        "    public function Init(self): ()\n    end\nend\n\nprint(PlayerData)\n"
    ));
    assert_eq!(
        m,
        "`@provider` requires `Init` to be private; `PlayerData` declares it public"
    );
}

#[test]
fn a_wrong_shape_names_what_the_file_wrote() {
    let m = one(concat!(
        "attribute provider on impl as\n",
        "    requires function Start(self)\n",
        "end\n\n",
        "struct PlayerData as\n    x: number\nend\n\n",
        "@provider\nimpl PlayerData as\n",
        "    function Start(self, dt: number): ()\n        print(dt)\n    end\nend\n\nprint(PlayerData)\n"
    ));
    assert_eq!(
        m,
        "`@provider` requires `Start(self)`; `PlayerData` declares `Start(self, dt: number)`"
    );
}

/// A clause with no shape asks for the member alone.
#[test]
fn a_clause_without_a_shape_takes_any_signature() {
    clean(concat!(
        "attribute service on impl as\n    requires function Start\nend\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\n    function Start(self, dt: number): ()\n        print(dt)\n    end\nend\n\nprint(S)\n"
    ));
}

/// `each` writes one clause per entry, and an empty list asks for none.
#[test]
fn each_reads_the_entries_of_the_argument() {
    let src = |list: &str, members: &str| {
        format!(
            "{LIFECYCLE}attribute provider(lifecycles: Lifecycle[]) on impl as\n    requires private function each lifecycles(self)\nend\n\nstruct S as\n    x: number\nend\n\n@provider({{ lifecycles = [{list}] }})\nimpl S as\n{members}end\n\nprint(S)\n"
        )
    };

    clean(&src(
        " Lifecycle.Init, Lifecycle.Start ",
        "    private function Init(self)\n    end\n\n    private function Start(self)\n    end\n",
    ));
    clean(&src(
        " Lifecycle.Start ",
        "    private function Start(self)\n    end\n",
    ));
    clean(&src("", ""));

    let m = one(&src(
        " Lifecycle.Init, Lifecycle.Start ",
        "    private function Start(self)\n    end\n",
    ));
    assert_eq!(
        m,
        "`@provider` requires a private function `Init(self)`; `S` declares none"
    );
}

/// A string entry names the member by its text, and the positional form
/// reads the same list as the record form.
#[test]
fn each_reads_a_string_entry_and_a_positional_argument() {
    clean(concat!(
        "attribute provider(lifecycles: string[]) on impl as\n",
        "    requires private function each lifecycles(self)\nend\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@provider([ \"Init\" ])\nimpl S as\n    private function Init(self)\n    end\nend\n\nprint(S)\n"
    ));
}

/// Two things are wrong at the declaration, not at a use.
#[test]
fn a_contract_on_a_target_with_no_members_reports_at_the_declaration() {
    let m = one(
        "attribute tag(name: string) on function as\n    requires public function Start(self)\nend\n",
    );
    assert!(m.starts_with("a `requires` clause reads the members of what the attribute sits on, and a function has none"), "{m}");
    assert_eq!(alloy::docs::kind_for(&m), "AttributeContract");
}

#[test]
fn each_needs_a_list_parameter() {
    let m = one(
        "attribute provider(lifecycles: string) on impl as\n    requires private function each lifecycles(self)\nend\n",
    );
    assert_eq!(
        m,
        "`each lifecycles` needs a list parameter; `lifecycles` is a `string`"
    );
}

#[test]
fn each_needs_a_parameter_of_that_name() {
    let m = one(
        "attribute provider(hooks: string[]) on impl as\n    requires private function each lifecycles(self)\nend\n",
    );
    assert_eq!(
        m,
        "`each lifecycles` needs a parameter named `lifecycles`; this attribute declares `hooks`"
    );
}

/// Every target that carries members takes a clause.
#[test]
fn the_targets_that_carry_members_all_read() {
    clean(concat!(
        "attribute a on struct as\n    requires field x: number\nend\n",
        "@a\nstruct S as\n    x: number\nend\n\nprint(S)\n"
    ));
    clean(concat!(
        "attribute a on interface as\n    requires field x: number\nend\n",
        "@a\ninterface I as\n    x: number\nend\n\nlocal v: I = { x = 1 }\nprint(v)\n"
    ));
    clean(concat!(
        "attribute a on trait as\n    requires function show(self): string\nend\n",
        "@a\ntrait Show as\n    function show(self): string\nend\n\nprint(Show)\n"
    ));
    clean(concat!(
        "attribute a on namespace as\n    requires public function boot()\n    requires private field seed: number\nend\n",
        "@a\nnamespace App as\n    private const seed: number = 1\n\n    public function boot()\n        print(seed)\n    end\nend\n\nprint(App)\n"
    ));
    clean(concat!(
        "attribute a on enum as\n    requires function label(self): string\nend\n",
        "@a\nenum E as\n    One\nend\n\nimpl E as\n    function label(self): string\n        return \"one\"\n    end\nend\n\nprint(E)\n"
    ));
}

/// A struct and its impl blocks are one type: a clause on either half
/// reads both.
#[test]
fn a_contract_reads_the_struct_and_its_impls_together() {
    clean(concat!(
        "attribute service on impl as\n    requires private field state: number\nend\n\n",
        "struct S as\n    private state: number\nend\n\n",
        "@service\nimpl S as\nend\n\nprint(S)\n"
    ));
    clean(concat!(
        "attribute service on struct as\n    requires public function Start(self)\nend\n\n",
        "@service\nstruct S as\n    x: number\nend\n\n",
        "impl S as\n    public function Start(self): ()\n    end\nend\n\nprint(S)\n"
    ));
}

/// The contract is a check: the emit is what it was.
#[test]
fn a_contract_emits_nothing_of_its_own() {
    let with = compile(concat!(
        "attribute service on impl as\n    requires public function Start(self)\nend\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\n    public function Start(self): ()\n    end\nend\n\nprint(S)\n"
    ));
    let without = compile(concat!(
        "attribute service on impl\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\n    public function Start(self): ()\n    end\nend\n\nprint(S)\n"
    ));
    assert!(with.diagnostics.is_empty(), "{:?}", with.diagnostics);
    assert!(
        with.ship
            .contains("__alloy.attribute(\"service\", { \"impl\" }, {  })"),
        "{}",
        with.ship
    );
    // The two artifacts differ only by the lines the body takes.
    let strip = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    };
    assert_eq!(strip(&with.ship), strip(&without.ship));
}

/// The short form is untouched: a declaration with no clause reads and
/// emits as it always did.
#[test]
fn the_short_form_keeps_its_reading() {
    clean(concat!(
        "attribute tag(name: string) on struct, enum\n\n",
        "@tag(\"core\")\nstruct S as\n    x: number\nend\n\nprint(S)\n"
    ));
}

/// A narrowed parameter takes the values it names, and reports the ones
/// it does not.
#[test]
fn a_narrowed_argument_takes_its_own_values() {
    clean(
        "attribute k(stage: \"a\" | \"b\") on struct\n\n@k(\"a\")\nstruct S as\n    x: number\nend\n\nprint(S)\n",
    );

    let m = one(
        "attribute k(stage: \"a\" | \"b\") on struct\n\n@k(\"z\")\nstruct S as\n    x: number\nend\n\nprint(S)\n",
    );
    assert_eq!(
        m,
        "the attribute `k` takes \"a\" | \"b\" for `stage`, `\"z\"` given"
    );

    let m = one(&format!(
        "{LIFECYCLE}attribute phases(names: (\"init\" | \"start\")[]) on struct\n\n@phases({{ names = [ \"init\", \"boot\" ] }})\nstruct S as\n    x: number\nend\n\nprint(S)\n"
    ));
    assert_eq!(
        m,
        "the attribute `phases` takes \"init\" | \"start\" for `names`, `\"boot\"` given"
    );
}

/// An enum-typed argument takes a variant of that enum.
#[test]
fn an_enum_argument_takes_a_variant() {
    clean(&format!(
        "{LIFECYCLE}attribute one(stage: Lifecycle) on struct\n\n@one(Lifecycle.Init)\nstruct S as\n    x: number\nend\n\nprint(S)\n"
    ));

    let m = one(&format!(
        "{LIFECYCLE}attribute one(stage: Lifecycle) on struct\n\n@one(Lifecycle.Boot)\nstruct S as\n    x: number\nend\n\nprint(S)\n"
    ));
    assert_eq!(
        m,
        "`Lifecycle` has no variant `Boot`; its variants are `Init` and `Start`"
    );
}

/// An entry the type rejects reports once: on the entry, not again as a
/// member the declaration lacks.
#[test]
fn a_rejected_entry_reports_once() {
    let m = one(&format!(
        "{LIFECYCLE}attribute provider(lifecycles: Lifecycle[]) on impl as\n    requires private function each lifecycles(self)\nend\n\nstruct S as\n    x: number\nend\n\n@provider({{ lifecycles = [ Lifecycle.Init, Lifecycle.Boot ] }})\nimpl S as\n    private function Init(self)\n    end\nend\n\nprint(S)\n"
    ));
    assert_eq!(
        m,
        "`Lifecycle` has no variant `Boot`; its variants are `Init` and `Start`"
    );
}

/// The gaps the editor writes: the member, where it goes, and how far in.
#[test]
fn a_gap_says_what_to_write_and_where() {
    let src = concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "    requires private field state: number\n",
        "end\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\nend\n\nprint(S)\n"
    );
    let out = compile(src);
    assert_eq!(out.contract_gaps.len(), 2, "{:?}", out.contract_gaps);

    let method = &out.contract_gaps[0];
    assert_eq!(method.attr, "service");
    assert_eq!(method.member, "Start");
    assert_eq!(method.kind, "function");
    assert_eq!(method.visibility, "public");
    assert_eq!(method.shape, "(self)");
    assert_eq!(method.indent, 0);
    // The method goes in front of the `end` of the `impl`.
    assert_eq!(&src[method.insert_at as usize..][..3], "end");
    assert!(
        src[..method.insert_at as usize].ends_with("impl S as\n"),
        "{}",
        &src[..method.insert_at as usize]
    );

    let field = &out.contract_gaps[1];
    assert_eq!(field.member, "state");
    assert_eq!(field.kind, "field");
    assert_eq!(field.visibility, "private");
    assert_eq!(field.shape, "number");
    // The field goes in the struct, which is where the language puts it.
    assert!(
        src[..field.insert_at as usize].ends_with("struct S as\n    x: number\n"),
        "{}",
        &src[..field.insert_at as usize]
    );
}

/// The members a gap names compile where the gap puts them.
#[test]
fn the_members_a_gap_names_compile() {
    let src = concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "    requires private field state: number\n",
        "end\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\nend\n\nprint(S)\n"
    );
    let out = compile(src);
    let mut text = src.to_string();
    let mut gaps = out.contract_gaps.clone();
    // From the bottom up, so an earlier offset still points at its byte.
    gaps.sort_by_key(|g| std::cmp::Reverse(g.insert_at));

    for gap in &gaps {
        let pad = " ".repeat(gap.indent as usize + 4);
        let member = match gap.kind.as_str() {
            "field" => format!("{pad}{} {}: {}\n", gap.visibility, gap.member, gap.shape),

            _ => format!(
                "{pad}{} function {}{}\n{pad}end\n",
                gap.visibility, gap.member, gap.shape
            ),
        };
        text.insert_str(gap.insert_at as usize - gap.indent as usize, &member);
    }

    let out = compile(&text);
    assert!(out.diagnostics.is_empty(), "{:?}\n{text}", out.diagnostics);
    assert!(out.contract_gaps.is_empty(), "{:?}", out.contract_gaps);
    // The written members read back the way the formatter writes them.
    let formatted = alloy::fmt::format(&text).expect("format");
    assert_eq!(formatted, text, "the fix is not formatted");
}

/// `requires` and `each` are names everywhere but a contract body.
#[test]
fn the_clause_words_stay_free_as_names() {
    for src in [
        "local requires = 1\nprint(requires)\n",
        "local each = 1\nprint(each)\n",
        "local t = { requires = 1, each = 2 }\nprint(t.requires, t.each)\n",
        "local function requires(x)\n    return x\nend\nprint(requires(1))\n",
        "local each = print\neach(1)\n",
    ] {
        clean(src);
    }
}

/// An attribute's hover lists what it requires.
#[test]
fn the_hover_lists_the_clauses() {
    let src = concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "    requires private field state: number\nend\n"
    );
    let decls = alloy::declarations::summaries(src, false);
    let hover = &decls
        .iter()
        .find(|d| d.name == "@service")
        .expect("the attribute")
        .hover;
    assert!(hover.contains("**Requires**"), "{hover}");
    assert!(hover.contains("- `public function Start(self)`"), "{hover}");
    assert!(hover.contains("- `private field state: number`"), "{hover}");
}

/// The hover keeps `each` for the declaration; the server expands it.
#[test]
fn the_hover_keeps_each_for_the_declaration() {
    let src = "attribute provider(lifecycles: Lifecycle[]) on impl as\n    requires private function each lifecycles(self)\nend\n";
    let decls = alloy::declarations::summaries(src, false);
    let hover = &decls
        .iter()
        .find(|d| d.name == "@provider")
        .expect("the attribute")
        .hover;
    assert!(
        hover.contains("- `private function each lifecycles(self)`"),
        "{hover}"
    );
    assert_eq!(
        alloy::declarations::attribute_params(hover),
        vec![("lifecycles".to_string(), "Lifecycle[]".to_string())]
    );
}

/// No message, no gap, and no hover names anything the emit invented.
#[test]
fn nothing_names_an_emit_only_name() {
    const EMIT_ONLY: &[&str] = &[
        "__alloy",
        "__ok",
        "__err",
        "_1",
        "@metatable",
        "*error-type*",
        "ResultOk",
    ];
    let src = concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "    requires private field state: number\nend\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\nend\n\nprint(S)\n"
    );
    let out = compile(src);
    let mut text: Vec<String> = out.diagnostics.iter().map(|d| d.message.clone()).collect();
    text.extend(
        out.contract_gaps
            .iter()
            .map(|g| format!("{} {} {} {}", g.attr, g.member, g.kind, g.shape)),
    );
    text.extend(
        alloy::declarations::summaries(src, false)
            .into_iter()
            .map(|d| d.hover),
    );

    for line in &text {
        for name in EMIT_ONLY {
            assert!(!line.contains(name), "`{name}` in {line:?}");
        }
    }
}

/// The examples the two doc entries show parse as written.
#[test]
fn the_doc_examples_parse() {
    for key in ["requires", "each"] {
        let text = alloy::docs::lookup(key).unwrap_or_else(|| panic!("no doc for `{key}`"));
        let mut fences = text.split("```");
        // The text opens with the fence, so the first split is empty and
        // the second holds the code.
        let _ = fences.next();
        let block = fences
            .next()
            .unwrap_or_else(|| panic!("no fence in `{key}`"));
        let code = block
            .strip_prefix("alloy\n")
            .unwrap_or_else(|| panic!("the fence of `{key}` is not alloy"));
        let lexed = alloy_syntax::lexer::lex(code)
            .unwrap_or_else(|e| panic!("`{key}`: lex at {}: {}", e.offset, e.message));
        alloy_syntax::parser::parse(code, &lexed.toks)
            .unwrap_or_else(|e| panic!("`{key}`: parse at {}: {}", e.offset, e.message));
    }
}

/// An attribute on a target it does not take reports once: on the
/// target, not again on every clause of its contract.
#[test]
fn a_wrong_target_reports_once() {
    let m = one(concat!(
        "attribute service on impl as\n    requires public function Start(self)\nend\n\n",
        "@service\nstruct S as\n    x: number\nend\n\nprint(S)\n"
    ));
    assert_eq!(
        m,
        "the attribute `service` has no meaning on a struct; it goes on `impl`"
    );
}
