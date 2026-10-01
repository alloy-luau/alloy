//! The emit of shapes that once built clean and shipped wrong Luau.

fn ship(src: &str) -> String {
    let out = alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    out.ship
}

fn messages(src: &str) -> Vec<String> {
    alloy::compile_with(src, &alloy::EmitOptions::default())
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn a_rename_that_is_no_name_goes_in_brackets() {
    let out = ship(
        "@derive(Serialize, Deserialize)\nstruct P as\n    @rename('regen-per-second')\n    regen: number\n    @rename(\"end\")\n    stop: number\nend\n",
    );
    assert!(out.contains("[\"regen-per-second\"] = self.regen"), "{out}");
    assert!(out.contains("regen = t[\"regen-per-second\"]"), "{out}");
    assert!(out.contains("[\"end\"] = self.stop"), "{out}");
}

/// A key is the text the literal stands for, so the quote style the
/// formatter picks and an escape change nothing.
#[test]
fn a_rename_reads_the_text_of_its_literal() {
    let single = ship(
        "@derive(Serialize, Deserialize)\nstruct P as\n    @rename('it\\'s')\n    a: number\nend\n",
    );
    let double = ship(
        "@derive(Serialize, Deserialize)\nstruct P as\n    @rename(\"it's\")\n    a: number\nend\n",
    );
    for out in [single, double] {
        assert!(out.contains("{ [\"it's\"] = self.a }"), "{out}");
        assert!(out.contains("P({ a = t[\"it's\"] })"), "{out}");
    }
}

/// A field of a struct that derives Serialize goes through that
/// struct's own pair, so the table nests and reads back with its
/// metatable.
#[test]
fn a_nested_struct_serializes_through_its_own_pair() {
    let out = ship(
        "@derive(Serialize, Deserialize)\nstruct Inner as\n    v: number\nend\n@derive(Serialize, Deserialize)\nstruct Outer as\n    inner: Inner\n    maybe: Inner?\nend\n",
    );
    assert!(out.contains("inner = Inner.to_table(self.inner)"), "{out}");
    // A key an older save lacks leaves the field to its default.
    assert!(
        out.contains("inner = (if t.inner == nil then nil else Inner.from_table(t.inner))"),
        "{out}"
    );
    assert!(
        out.contains("maybe = if self.maybe == nil then nil else Inner.to_table(self.maybe)"),
        "{out}"
    );
}

/// `from_table` copied an enum field as it was, so `"Middle"` read as a
/// `Tier`. Each enum read now goes through the std check, which names
/// the struct and the field; a payload enum passes its metatable, and
/// an array checks each item.
#[test]
fn an_enum_field_reads_back_through_the_variant_check() {
    let out = ship(
        "import { Deserialize } from \"@alloy/std/serde\"\nenum Tier as\n    Low\n    High\nend\nenum Shape as\n    Circle(number)\n    Empty\nend\n@derive(Deserialize)\nstruct Card as\n    tier: Tier\n    shape: Shape\n    maybe: Tier?\n    all: Tier[]\nend\n",
    );
    for want in [
        "tier = (if t.tier == nil then nil else __alloy.serde_variant(t.tier, { Low = 0, High = 0 }, nil, \"Tier\", \"Card.tier\"))",
        "shape = (if t.shape == nil then nil else __alloy.serde_variant(t.shape, { Circle = 1, Empty = 0 }, Shape, \"Shape\", \"Card.shape\"))",
        "maybe = if t.maybe == nil then nil else __alloy.serde_variant(t.maybe, { Low = 0, High = 0 }, nil, \"Tier\", \"Card.maybe\")",
        "__alloy.Array.map(t.all, function(_v0) return __alloy.serde_variant(_v0, { Low = 0, High = 0 }, nil, \"Tier\", \"Card.all\") end)",
    ] {
        assert!(out.contains(want), "{want}\n{out}");
    }
}

#[test]
fn a_derive_reports_a_key_or_a_name_it_cannot_hold() {
    assert_eq!(
        messages(
            "@derive(Serialize, Clone)\nstruct D as\n    @rename(\"a\")\n    x: number\n    @rename(\"a\")\n    y: number\n    clone: number\nend\n"
        ),
        vec![
            "`x` and `y` serialize under one key, `a`, and the derived table keeps one; give one of them another `@rename`",
            "`clone` is a field of `D` and a method `@derive(Clone)` writes; one name holds one of the two",
        ]
    );
}

/// `continue` is a statement arm, and an attribute with arguments reads
/// on a method as it does on a function.
#[test]
fn a_method_takes_an_attribute_with_arguments() {
    let out = ship(
        "attribute route(path: string) on function\nstruct Svc as\n    n: number\nend\nimpl Svc as\n    @deprecated(\"use run2\")\n    function run(self): number\n        return self.n\n    end\n    @route(\"/x\")\n    function run2(self): number\n        return self.n\n    end\nend\nfor i = 1, 3 do\n    match i with\n        case 2 then continue\n        default print(i)\n    end\nend\n",
    );
    assert!(
        out.contains("@[deprecated {reason = \"use run2\"}]"),
        "{out}"
    );
    assert!(
        out.contains("end __alloy.attach(Svc.run2, { route = { \"/x\" } })"),
        "{out}"
    );
    assert!(out.contains("then continue"), "{out}");
}

#[test]
fn an_enum_reports_the_names_its_emit_takes() {
    assert_eq!(
        messages("enum E as\n    is(number)\n    Other\nend\n"),
        vec!["a variant cannot be named `is`; the enum's type test `E.is(v)` takes that name"]
    );
    assert_eq!(
        messages("enum E as\n    A\n    A\nend\n"),
        vec!["`A` is already a variant of this enum"]
    );
    assert_eq!(
        messages(
            "enum Ev as\n    Hit\nend\nimpl Ev as\n    function tag(self)\n        return 1\n    end\nend\n"
        ),
        vec![
            "an enum method cannot be named `tag`; each variant keeps its name in the field `tag`"
        ]
    );
}

#[test]
fn a_temp_skips_the_names_the_source_uses() {
    let out = ship("local _1 = 5\nlocal t = {}\nlocal x = t?.a ?? 0\nprint(_1, x)\n");
    assert_eq!(out.matches("local _1").count(), 1, "{out}");
}

#[test]
fn a_default_that_reads_a_field_reports() {
    assert_eq!(
        messages("struct P as\n    w: number = 1\n    h: number = w * 3\nend\n"),
        vec![
            "a default cannot read the field `w`; the constructor fills defaults before the fields exist"
        ]
    );
    assert!(
        messages("local w = 2\nstruct P as\n    w: number = 1\n    h: number = w * 3\nend\n")
            .is_empty()
    );
}

#[test]
fn a_deprecated_message_goes_in_the_reason_table() {
    let out = ship("@deprecated(\"use g\")\nlocal function f() end\nf()\n");
    assert!(out.contains("@[deprecated {reason = \"use g\"}]"), "{out}");
}

#[test]
fn a_method_call_on_a_mixed_enum_names_the_string_variant() {
    assert_eq!(
        messages(
            "enum State as\n    Idle\n    Running(number)\nend\nimpl State as\n    function label(self): string\n        return \"x\"\n    end\nend\nlocal function describe(s: State): string\n    return s:label()\nend\nprint(describe(State.Idle))\n"
        ),
        vec!["`State.Idle` is a unit variant, a string at runtime; call `State.label(s)`"]
    );
}

/// A `.d.aly` declares globals. Luau makes a type of a definitions file
/// global only when it says `export`, so `interface Iface` stayed out of
/// every file's reach. An enum wrote its runtime table into the file,
/// and a trailing `return` followed.
#[test]
fn a_definitions_file_declares_every_type_global_and_runs_nothing() {
    let options = alloy::EmitOptions {
        file_name: "g.d.aly".to_string(),
        definitions: true,
        ..alloy::EmitOptions::default()
    };
    let src = "type Plain = number\nexport type Id = string\ninterface Iface as\n    a: number\nend\nstruct Rec as\n    c: number\nend\nenum Mode as Fast, Slow end\nenum Shape as\n    Circle(number),\n    Dot,\nend\ndeclare function area(s: Shape): Plain\n";
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        out.check.lines().count(),
        src.lines().count(),
        "{}",
        out.check
    );

    for line in [
        "export type Plain = number",
        "export type Id = string",
        "export type Iface = { a: number }",
        "export type Rec = { c: number }",
        "export type Mode = \"Fast\" | \"Slow\"",
        "export type Shape = { tag: \"Circle\", _1: number } | \"Dot\"",
    ] {
        assert!(out.check.contains(line), "{line}\n{}", out.check);
    }

    assert!(!out.check.contains("local "), "{}", out.check);
    assert!(!out.check.contains("return"), "{}", out.check);
}

/// A definitions file runs no require, so a std type there is unknown to
/// the checker, and one unknown type drops the whole file. `number[]`
/// lowered to a bare `Array<number>` and broke every declaration beside
/// it. A host gives a plain table, so the array is a Luau array.
#[test]
fn a_definitions_file_writes_arrays_as_tables_and_names_a_std_type() {
    let options = alloy::EmitOptions {
        file_name: "g.d.aly".to_string(),
        definitions: true,
        ..alloy::EmitOptions::default()
    };
    let src = "declare items: number[]\ndeclare names: read string[]\ninterface Bag as\n    slots: Bag[]\nend\n";
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    for line in [
        "declare items: { number }",
        "declare names: { read [number]: string }",
        "export type Bag = { slots: { Bag } }",
    ] {
        assert!(out.check.contains(line), "{line}\n{}", out.check);
    }

    assert!(!out.check.contains("Array"), "{}", out.check);

    let std = "declare function load(): Result<number, string>\n";
    let messages: Vec<String> = alloy::compile_with(std, &options)
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        [
            "`Result` is a type of the Alloy std, and a definitions file cannot reach the std; write the type out"
        ]
    );
}

/// The statement form sets `Parent` after every other field, as the
/// runtime `init` of the expression form does. The value still runs in
/// its own place, and each line keeps its own line.
#[test]
fn an_initializer_sets_parent_last() {
    let src = "local p = new Instance('Part') {\n    Parent = workspace,\n    Name = 'Box',\n}\nnew Instance('Part') {\n    Parent = p,\n    Name = 'Lid',\n}\n";
    let out = ship(src);
    assert!(
        out.contains("local _parent1 = workspace \n    p.Name = 'Box' \np.Parent = _parent1"),
        "{out}"
    );
    assert!(
        out.contains("local _parent3 = p \n    _n2.Name = 'Lid' \n_n2.Parent = _parent3"),
        "{out}"
    );
    assert_eq!(out.lines().count(), src.lines().count());

    // A `Parent` already last stays in place, with no temp.
    let last =
        ship("local p = new Instance('Part') {\n    Name = 'Box',\n    Parent = workspace,\n}\n");
    assert!(last.contains("p.Parent = workspace"), "{last}");
    assert!(!last.contains("_parent"), "{last}");
}

/// A module of `count` exports whose values are no constants, so each
/// would take a register, a function that reads some of them, and a
/// local of the same name inside it.
fn many_exports(count: usize) -> String {
    let mut src = String::new();

    for i in 0..count {
        src.push_str(&format!(
            "--- Constant {i}.\nexport const C{i} = tostring({i})\n"
        ));
    }

    src.push_str(
        "--- Joins three constants.\nexport function joined(): string\n    local C2 = \"own\"\n    return C1 .. C2 .. C0\nend\n--- A value from two constants.\nexport const BOTH = C0 .. C1\nlocal seen = `{C3}`\nprint(seen)\n",
    );

    src
}

/*
Luau holds at most 200 locals in one function, the chunk included, and
each `export const` built to a local. A constants file of 219 built and
checked clean, then failed to load in Studio (LANG_BUGS 107). Past the
budget the exported values live on one table, every reference in the
module reads the table, and a local of the same name still shadows it.
The module loads, and a module under the budget builds as before.
*/
#[test]
fn a_module_past_the_local_budget_puts_its_exports_on_a_table() {
    let src = many_exports(250);
    let out = alloy::compile_file(
        "constants.aly",
        &src,
        &alloy::EmitOptions::default(),
        None,
        None,
    )
    .unwrap();

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.ship.lines().count(), src.lines().count());
    assert_eq!(out.check.lines().count(), src.lines().count());

    for want in [
        "local __exports = {} ",
        "__exports.C249 = tostring(249)",
        "    return __exports.C1 .. C2 .. __exports.C0",
        "__exports.BOTH = __exports.C0 .. __exports.C1",
        "local seen = `{__exports.C3}`",
        " __exports.joined = joined return __exports",
    ] {
        assert!(out.ship.contains(want), "missing {want:?}");
    }

    let lua = mlua::Lua::new();
    let exports: mlua::Table = lua.load(out.ship.as_str()).eval().unwrap();
    assert_eq!(exports.get::<String>("C249").unwrap(), "249");
    assert_eq!(exports.get::<String>("BOTH").unwrap(), "01");
    let joined: mlua::Function = exports.get("joined").unwrap();
    assert_eq!(joined.call::<String>(()).unwrap(), "1own0");

    // Under the budget nothing moves.
    let small = alloy::compile_with(&many_exports(20), &alloy::EmitOptions::default()).unwrap();
    assert!(!small.ship.contains("__exports"), "{}", small.ship);
    assert!(small.ship.contains("return { C0 = C0,"), "{}", small.ship);
}

/// The annotation of a moved export types its field in the check
/// artifact, so a value of another type still reports, and a module
/// that imports one reads the type.
#[test]
fn a_moved_export_keeps_its_type() {
    let dir = std::env::temp_dir().join(format!("alloy-exports-table-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/constants.aly"),
        many_exports(250) + "--- A count.\nexport const COUNT: number = 3\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/use.aly"),
        "import { C7, COUNT, joined } from './constants'\n\nconst n: number = C7\nconst m: number = COUNT + #joined()\nprint(n, m)\n",
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    if alloy::typecheck::find_luau_lsp(&config.flux).is_none() {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    }

    let analysis = alloy::typecheck::analyze(&dir, &config, &report.checks, &report.dep_artifacts)
        .expect("the type check runs");
    let errors: Vec<String> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{}:{} {}", d.rel.display(), d.line, d.message))
        .collect();

    assert_eq!(
        errors,
        vec!["use.aly:3 Expected this to be 'number', but got 'string'".to_string()]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/*
A provider that holds a struct with HashMaps, through a field typed in
another module, checks clean. The check artifact wrote an `any` default
into the field and returned an `any` as the struct, and luau-lsp solved
the whole class of the field for each one. Three reports of its limit
came back (LANG_BUGS 117). A default of the wrong type still reports.
*/
#[test]
fn a_provider_that_holds_hashmaps_checks_clean() {
    let dir = std::env::temp_dir().join(format!("alloy-held-maps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let files = [
        ("alloy.toml", "[build]\nin = \"src\"\nout = \"build\"\n"),
        (
            "src/use.aly",
            "--- Any provider by name, as Forge.use gives it.\nexport function use(name: string) -> any\n  return name\nend\n",
        ),
        (
            "src/chests.aly",
            "import { HashMap } from '@alloy/std/collections'\n\n--- The chests.\nexport struct Chests\n  m0: HashMap<number, { number }> = new HashMap()\n  m1: HashMap<number, { number }> = new HashMap()\nend\n",
        ),
        (
            "src/world.aly",
            "import { HashMap } from '@alloy/std/collections'\nimport { Chests } from './chests'\n\n--- An open world.\nexport struct Open\n  chests: Chests\nend\n\n--- Holds the world.\nexport struct WorldProvider\n  private open: Open? = nil\n  private h0: HashMap<string, { number }> = new HashMap()\n  private h1: HashMap<string, { number }> = new HashMap()\n  private h2: HashMap<string, { number }> = new HashMap()\nend\n",
        ),
        (
            "src/session.aly",
            "import { use } from './use'\nimport { WorldProvider } from './world'\n\n--- Holds a provider.\nexport struct SessionProvider\n  private worlds: WorldProvider = use('WorldProvider')\nend\n",
        ),
        (
            "src/bad.aly",
            "--- A default of the wrong type.\nexport struct Bad\n  n: number = 'no'\nend\n",
        ),
    ];

    for (path, text) in files {
        std::fs::write(dir.join(path), text).unwrap();
    }

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    if alloy::typecheck::find_luau_lsp(&config.flux).is_none() {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    }

    let analysis = alloy::typecheck::analyze(&dir, &config, &report.checks, &report.dep_artifacts)
        .expect("the type check runs");
    let errors: Vec<String> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{}:{} {}", d.rel.display(), d.line, d.message))
        .collect();

    assert_eq!(
        errors,
        vec!["bad.aly:2 Expected this to be 'number', but got 'string'".to_string()]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/*
A limit of the Luau compiler fails the module at load in Roblox, and
nothing else saw it: the build, the analyzer, and `luau-compile --null`
passed. The ship goes through the compiler, and a limit reports at the
line of the declaration that crosses it. The check counts a local that
holds a constant, as Studio did, where `luau-compile` folds it away.
*/
#[test]
fn a_luau_compiler_limit_reports_at_its_declaration() {
    let compile = |src: &str| {
        alloy::compile_file(
            "limits.aly",
            src,
            &alloy::EmitOptions::default(),
            None,
            None,
        )
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| {
            let (line, _) = alloy::directives::line_col(src, d.start as usize);

            (line, alloy::docs::kind_for(&d.message), d.message)
        })
        .collect::<Vec<_>>()
    };

    let mut locals = String::from("local function many(): number\n");

    for i in 0..210 {
        // `const` too: the compiler here reads it as `local`.
        locals.push_str(&format!("    const v{i} = tostring({i})\n"));
    }

    locals.push_str("    return #v0 + #v209\nend\nprint(many())\n");
    let got = compile(&locals);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, 202, "{got:?}");
    assert_eq!(got[0].1, "LuauLimit");
    assert!(
        got[0].2.starts_with("Luau cannot compile this module: out of local registers when trying to allocate `v200`: exceeded limit 200; one function holds at most 200 locals"),
        "{got:?}"
    );

    // Locals that each hold a constant count too: Studio refused a
    // module of 219 of them.
    let folded: String = (0..210).map(|i| format!("local k{i} = {i}\n")).collect();
    let got = compile(&(folded + "print(k0, k209)\n"));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, 201, "{got:?}");

    let mut upvalues = String::new();

    for i in 0..210 {
        upvalues.push_str(&format!("local u{i} = tostring({i})\n"));
    }

    let reads: Vec<String> = (0..210).map(|i| format!("#u{i}")).collect();
    upvalues.push_str(&format!(
        "local function wide(): number\n    return {}\nend\nprint(wide())\n",
        reads.join(" + ")
    ));
    let got = compile(&upvalues);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].2.contains("out of upvalue registers"), "{got:?}");
    assert!(got[0].2.contains("read some through a table"), "{got:?}");
}
