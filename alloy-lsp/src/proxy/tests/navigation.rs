//! An imported name in the editor: where it is declared, and what a
//! rename of it writes.
//!
//! The emit binds a name from an import list in generated text, so the
//! child answers about a byte no author wrote. Its ranges came back one
//! character wide, in the module as often as the using file.

use super::super::navigation::{
    Target, export_span, impl_method_span, import_entries, module_bindings, module_head_line,
    name_uses, trait_method_span, uses_in_range,
};
use super::super::*;

const MODULE: &str = concat!(
    "--- Marks a struct.\n",
    "export attribute tagged(name: string) on struct\n",
    "\n",
    "--- What the module holds.\n",
    "export type Held = { n: number }\n",
    "\n",
    "--- A helper.\n",
    "export function make(n: number): number\n",
    "    return n\n",
    "end\n",
    "\n",
    "--- The version.\n",
    "export const version = 3\n",
);

const USER: &str = concat!(
    "import { version, make, type Held, @tagged } from \"./m\"\n",
    "\n",
    "--- Marked.\n",
    "@tagged(\"a\")\n",
    "struct S as\n",
    "    n: number,\n",
    "end\n",
    "\n",
    "--- Reads them.\n",
    "export function read(): number\n",
    "    local h: Held = { n = version }\n",
    "\n",
    "    return make(h.n) + version + new S { n = 1 }.n\n",
    "end\n",
);

const ALIASED: &str = concat!(
    "import { version as ver } from \"./m\"\n",
    "\n",
    "--- Reads it under another name.\n",
    "export function twice(): number\n",
    "    return ver + ver\n",
    "end\n",
);

const STARRED: &str = concat!(
    "import * as M from \"./m\"\n",
    "\n",
    "--- Reads it through the module.\n",
    "export function thrice(): number\n",
    "    return M.version + M.make(1)\n",
    "end\n",
);

/// Every part of an import list, with the byte range of each name.
#[test]
pub(crate) fn an_import_list_reads_the_range_of_every_name() {
    let found = import_entries(USER);
    let at = |name: &str| found.iter().find(|e| e.name == name).expect(name);
    let text = |(s, e): (usize, usize)| &USER[s..e];

    assert_eq!(found.len(), 4, "{found:?}");
    assert_eq!(text(at("version").name_at), "version");
    assert_eq!(text(at("make").name_at), "make");
    assert_eq!(text(at("Held").name_at), "Held");
    // The `@` is how the name is written, not part of it. A rename
    // edits the name and leaves the sigil.
    assert_eq!(text(at("tagged").name_at), "tagged");
    assert_eq!(
        &USER[at("tagged").name_at.0 - 1..at("tagged").name_at.0],
        "@"
    );

    for name in ["version", "make", "Held", "tagged"] {
        assert_eq!(at(name).bound, name);
        assert_eq!(at(name).alias_at, None);
        assert_eq!(at(name).spec, "./m");
    }

    // `version as ver`: the entry holds both names, and the file writes
    // the alias.
    let aliased = import_entries(ALIASED);
    assert_eq!(aliased.len(), 1);
    assert_eq!(aliased[0].name, "version");
    assert_eq!(aliased[0].bound, "ver");
    assert_eq!(
        &ALIASED[aliased[0].name_at.0..aliased[0].name_at.1],
        "version"
    );

    let (s, e) = aliased[0].alias_at.expect("the alias");
    assert_eq!(&ALIASED[s..e], "ver");

    // A list over several lines reads the same: the walk runs to the
    // `from` of the statement and not to the end of the line.
    const WRAPPED: &str = concat!(
        "import {\n",
        "    version,\n",
        "    make as build,\n",
        "} from \"./m\"\n",
        "\n",
        "local t = { n = 1 }\n",
    );
    let wrapped = import_entries(WRAPPED);

    assert_eq!(wrapped.len(), 2, "{wrapped:?}");
    assert_eq!(
        &WRAPPED[wrapped[0].name_at.0..wrapped[0].name_at.1],
        "version"
    );
    assert_eq!(wrapped[0].spec, "./m");
    assert_eq!(wrapped[1].bound, "build");

    let (s, e) = wrapped[1].alias_at.expect("the alias");
    assert_eq!(&WRAPPED[s..e], "build");

    // A table literal under an import with no list is nobody's entry.
    assert!(import_entries("import M from \"./m\"\nlocal t = { n = 1 }\n").is_empty());

    // `import * as M` binds a module and no name out of a list.
    assert!(import_entries(STARRED).is_empty());
    assert_eq!(
        module_bindings(STARRED),
        [("M".to_string(), "./m".to_string())]
    );
    // A default binding reads the `default` field of an export table,
    // so `M.name` there is a field of that value.
    assert!(module_bindings("import M from \"./m\"\n").is_empty());
}

/// Where a module declares what it exports, whatever the keyword.
#[test]
pub(crate) fn an_export_reads_the_range_of_its_own_name() {
    let at = |name: &str| {
        let (s, e) = export_span(MODULE, name).expect(name);

        (&MODULE[s..e], MODULE[..s].matches('\n').count())
    };

    assert_eq!(at("tagged"), ("tagged", 1));
    assert_eq!(at("Held"), ("Held", 4));
    assert_eq!(at("make"), ("make", 7));
    assert_eq!(at("version"), ("version", 12));
    assert_eq!(export_span(MODULE, "missing"), None);
}

/// A project on disk, so a spec resolves and the walk reaches the
/// module. The files carry the names the tests above read.
fn project(name: &str) -> (State, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("alloy-nav-{name}-{}", std::process::id()));
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

    for (rel, src) in [
        ("m.aly", MODULE),
        ("use.aly", USER),
        ("other.aly", ALIASED),
        ("star.aly", STARRED),
    ] {
        let path = dir.join("src").join(rel);
        std::fs::write(&path, src).expect(rel);
        let uri = format!("file://{}", path.display());
        let options = EmitOptions {
            file_name: path.to_string_lossy().into_owned(),
            ..EmitOptions::default()
        };
        st.docs.insert(
            uri,
            Doc::new(
                src.to_string(),
                1,
                &options,
                &alloy::luaux::Config::default(),
                None,
            ),
        );
    }

    (st, dir)
}

/// The edits of one workspace edit, as `file line:col-col -> text`, in
/// a stable order.
fn rows(edit: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let changes = edit["changes"].as_object().expect("changes");

    for (uri, edits) in changes {
        let file = uri.rsplit('/').next().unwrap_or(uri).to_string();

        for e in edits.as_array().expect("edits") {
            let r = &e["range"];

            out.push(format!(
                "{file} {}:{}-{} -> {}",
                r["start"]["line"],
                r["start"]["character"],
                r["end"]["character"],
                e["newText"].as_str().unwrap_or("")
            ));
        }
    }

    out.sort();
    out
}

/// Go to definition on a name in an import list landed nowhere: the
/// emit binds it in generated text, so the child had no byte to point
/// at. The module declares it, and that is the answer for the entry,
/// for the alias, for a use, and for `M.name`.
#[test]
pub(crate) fn a_name_from_an_import_list_finds_its_declaration() {
    let (st, dir) = project("definition");
    let uri = |rel: &str| format!("file://{}", dir.join("src").join(rel).display());
    let line = |result: &Value| result[0]["range"]["start"]["line"].as_u64().unwrap();
    // The caret on the last character of the text, which every case
    // below ends inside the name with.
    let at = |rel: &str, src: &str, text: &str, occurrence: usize| {
        let mut offset = 0;

        for _ in 0..occurrence {
            offset += src[offset..].find(text).expect(text) + 1;
        }

        let offset = offset + text.len() - 2;

        st.import_name_definition(&uri(rel), src, offset)
            .unwrap_or(Value::Null)
    };

    // Each one lands on the declaring line of `src/m.aly`.
    assert_eq!(line(&at("use.aly", USER, "{ version", 1)), 12);
    assert_eq!(line(&at("use.aly", USER, ", make", 1)), 7);
    assert_eq!(line(&at("use.aly", USER, "@tagged", 1)), 1);
    assert_eq!(line(&at("use.aly", USER, "type Held", 1)), 4);
    // A use under the list reads the same.
    assert_eq!(line(&at("use.aly", USER, "n = version", 1)), 12);
    assert_eq!(line(&at("use.aly", USER, "@tagged", 2)), 1);
    // Both halves of an aliased entry, and a use of the alias.
    assert_eq!(line(&at("other.aly", ALIASED, "{ version", 1)), 12);
    assert_eq!(line(&at("other.aly", ALIASED, "as ver", 1)), 12);
    assert_eq!(line(&at("other.aly", ALIASED, "return ver", 1)), 12);
    // `M.version` under `import * as M`.
    assert_eq!(line(&at("star.aly", STARRED, "M.version", 1)), 12);

    // A name no import list binds is the child's to answer.
    assert!(at("use.aly", USER, "struct S", 1).is_null());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A rename of an imported name reaches the declaration, every import
/// list, every use under an unaliased entry, and every `M.name`. An
/// entry with an alias keeps its own name.
#[test]
pub(crate) fn a_rename_of_an_imported_name_reaches_every_file() {
    let (st, dir) = project("rename");
    let file = dir.join("src/m.aly");
    let edit = st
        .export_rename(&file, "version", "release")
        .expect("the export answers");

    assert_eq!(
        rows(&edit),
        [
            "m.aly 12:13-20 -> release",
            "other.aly 0:9-16 -> release",
            "star.aly 4:13-20 -> release",
            "use.aly 0:9-16 -> release",
            "use.aly 10:26-33 -> release",
            "use.aly 12:23-30 -> release",
        ]
    );

    // The alias file changes its entry and nothing else: `ver` is its
    // own word, and no other file knows it.
    let edit = st
        .export_rename(&file, "tagged", "marked")
        .expect("the attribute answers");

    assert_eq!(
        rows(&edit),
        [
            "m.aly 1:17-23 -> marked",
            "use.aly 0:36-42 -> marked",
            "use.aly 3:1-7 -> marked",
        ]
    );

    // A name the module does not export has no rename of this kind.
    assert!(st.export_rename(&file, "missing", "other").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The emit writes the `local` of a star import itself, so the child
/// points at generated text. The import line binds the name.
#[test]
fn a_star_alias_opens_at_its_import_line() {
    let src = "import * as M from \"./mod\"\n\nlocal x: M.Ns.T = new M.Ns.T { value = 1 }\n";
    let uri = "file:///w/src/main.aly";
    let found = module_binding_definition(src, uri, "M").expect("the binding");

    assert_eq!(
        found,
        json!([{
            "uri": uri,
            "range": {
                "start": { "line": 0, "character": 12 },
                "end": { "line": 0, "character": 13 },
            },
        }])
    );
    // A name no import binds this way answers nothing.
    assert_eq!(module_binding_definition(src, uri, "x"), None);
}

/// `Outer.Inner.T`: the word in front of the group is a group of this
/// file, not a module binding. The member is the declaring file's own,
/// so go-to-definition and rename read it like any other member.
#[test]
fn a_member_two_groups_deep_resolves_in_its_own_file() {
    let src = "export namespace Outer as\n    namespace Inner as\n        struct T as\n            value: number,\n        end\n    end\nend\n\nlocal z: Outer.Inner.T = new Outer.Inner.T { value = 9 }\n";
    let uri = "file:///w/src/nested.aly";
    let st = super::support::files(&[(uri, src)]);
    let at = src.rfind("Inner.T").expect("the path") + "Inner.".len();
    let (file, name) = st
        .module_member_at(uri, src, at)
        .expect("the member the path names");

    assert_eq!(file, Path::new("/w/src/nested.aly"));
    assert_eq!(name, "T");

    // A word in front that names no group of the file answers nothing.
    let other = "local p = { value = 1 }\nprint(p.value)\n";
    let st = super::support::files(&[("file:///w/src/plain.aly", other)]);

    assert_eq!(
        st.module_member_at(
            "file:///w/src/plain.aly",
            other,
            other.rfind("value").expect("the field")
        ),
        None
    );
}

/// The child indexes the check artifact, where a namespace member is
/// `Ns_T` and carries a `__new` and a `new` the author never wrote. An
/// Alloy file answers a workspace query from its own source.
#[test]
fn a_workspace_symbol_of_an_alloy_file_reads_the_source() {
    let st = super::support::files(&[(
        "file:///w/src/mod.aly",
        "export namespace Ns as\n    struct T as\n        value: number,\n    end\nend\n",
    )]);
    let mut out: Vec<Value> = Vec::new();
    source_symbols(&st, Some("Ns"), &mut out);
    let names: Vec<&str> = out
        .iter()
        .filter_map(|s| s.get("name").and_then(Value::as_str))
        .collect();

    assert_eq!(names, vec!["Ns", "Ns.T", "Ns.T.value"]);

    // The query names the member, not the emit.
    let mut out: Vec<Value> = Vec::new();
    source_symbols(&st, Some("Ns_T"), &mut out);

    assert!(out.is_empty(), "{out:?}");

    let mut out: Vec<Value> = Vec::new();
    source_symbols(&st, Some("T"), &mut out);
    let names: Vec<&str> = out
        .iter()
        .filter_map(|s| s.get("name").and_then(Value::as_str))
        .collect();

    assert_eq!(names, vec!["Ns.T", "Ns.T.value"]);
}

/// `.ember` holds the packages a require reaches and `.alloy` the
/// build's sourcemap, so both stand in the mirror the child indexes.
/// Their modules are no source the reader wrote, and `workspace/symbol`
/// used to list them while `node_modules` and the build output stayed
/// out.
#[test]
fn a_dot_directory_holds_no_workspace_symbol() {
    let mirror = Path::new("/m");
    let root = Path::new("/w");
    let dotted = |path: &str| in_a_dot_directory(Path::new(path), mirror, Some(root));

    assert!(dotted("/m/.ember/widget.luau"));
    assert!(dotted("/m/.alloy/decoy.luau"));
    assert!(dotted("/w/.vscode/settings.luau"));
    assert!(!dotted("/m/src/game.luau"));
    assert!(!dotted("/w/src/game.aly"));
    // A file whose own name opens with a dot declares nothing.
    assert!(!dotted("/m/.luaurc"));
    // A project under a dot directory of the home tree is still the
    // project: only the path below the root decides.
    assert!(!in_a_dot_directory(
        Path::new("/home/a/.games/w/src/game.aly"),
        mirror,
        Some(Path::new("/home/a/.games/w")),
    ));
}

/// A binding that holds a whole module opens the module's own file:
/// `import * as M` and the default binding of a plain Luau module, which
/// declares no `export default`.
#[test]
fn a_whole_module_binding_opens_at_its_return() {
    assert_eq!(
        module_head_line(
            "local M = {}\n\nfunction M.zero(): number\n    return 0\nend\n\nreturn M\n"
        ),
        6
    );
    // An Alloy module hands nothing back by a `return`, so its file
    // opens at the first line.
    assert_eq!(module_head_line("export const LIMIT = 100\n"), 0);
    // A `return` inside a function body is not the module's.
    assert_eq!(
        module_head_line("local function f()\n    return 1\nend\n"),
        0
    );
}

/// `boxed:get()`: the receiver is a local, so the emit writes the method
/// on the target's table and the child landed on the `end` of the
/// generic struct's header. A foreign target answered nothing at all.
#[test]
fn an_impl_declares_the_method_a_receiver_calls() {
    let src = concat!(
        "struct Box<T> as\n",
        "    value: T\n",
        "end\n",
        "\n",
        "impl Box<T> as\n",
        "    function new(value: T): Box<T>\n",
        "        return new Box { value = value }\n",
        "    end\n",
        "\n",
        "    function get(self): T\n",
        "        return self.value\n",
        "    end\n",
        "end\n",
        "\n",
        "export impl string as\n",
        "    function shout(self): string\n",
        "        return string.upper(self)\n",
        "    end\n",
        "end\n",
    );
    let span = |name: &str| impl_method_span(src, name).map(|(a, _)| a);

    // A blank line inside the block closes nothing.
    assert_eq!(span("get"), Some(src.find("get(self)").expect("get")));
    assert_eq!(span("shout"), Some(src.find("shout(self)").expect("shout")));
    // `Box.new` is written at the call site and reaches the declaration
    // path instead; it takes no `self`.
    assert_eq!(span("new"), None);
    // A name outside every block is no method.
    assert_eq!(span("value"), None);
}

/// `self:area()` inside a trait's own default method. Every
/// implementation writes `function area(self)` again, so the impl scan
/// found several and answered with an arbitrary file. The trait declares
/// the method once.
#[test]
pub(crate) fn a_trait_default_method_reaches_the_traits_own_signature() {
    let src = concat!(
        "trait Shape as\n",
        "    function area(self): number\n",
        "\n",
        "    function describe(self): string\n",
        "        return `area {self:area()}`\n",
        "    end\n",
        "end\n",
        "\n",
        "impl Shape for Circle as\n",
        "    function area(self): number\n",
        "        return 1\n",
        "    end\n",
        "end\n",
    );

    assert_eq!(
        trait_method_span(src, 4, "area").map(|(a, _)| a),
        Some(src.find("area(self): number").expect("area"))
    );
    // Outside the trait body, and for a name the trait does not write.
    assert_eq!(trait_method_span(src, 9, "area"), None);
    assert_eq!(trait_method_span(src, 4, "radius"), None);
}

/*
A type annotation is a use of the name; `obj:method(...)` is not.

`references` and `rename` walk one list, and it left the annotation of
`local function describe(v: Vec2)` out: the `:` before the name read as
the receiver of a method call.
*/
#[test]
pub(crate) fn an_annotation_is_a_use_and_a_method_call_is_not() {
    let src = concat!(
        "local a = new Vec2 { x = 1 }\n",
        "local function describe(v: Vec2): number\n",
        "    return a:len()\n",
        "end\n",
    );
    let at = |needle: &str| src.find(needle).expect(needle);
    assert_eq!(
        name_uses(src, "Vec2"),
        vec![
            (at("Vec2 {"), at("Vec2 {") + 4),
            (at("Vec2):"), at("Vec2):") + 4),
        ]
    );

    // The receiver of a call after `:` names a member, not a type.
    assert_eq!(name_uses(src, "len"), Vec::new());
}

/// One walk answers a rename and a reference list, so the caret on the
/// declaration, on the import, and on a use all name the same export,
/// and the annotation is one of the places the rename writes.
#[test]
pub(crate) fn one_target_answers_the_rename_and_the_references() {
    let module = "export struct Vec2 as\n    x: number\nend\n";
    let user = concat!(
        "import { Vec2 } from \"./m2\"\n",
        "local a = new Vec2 { x = 1 }\n",
        "local function describe(v: Vec2): number\n",
        "    return v.x\n",
        "end\n",
    );
    let dir = std::env::temp_dir().join(format!("alloy-nav-target-{}", std::process::id()));
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

    for (rel, src) in [("m2.aly", module), ("u2.aly", user)] {
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

    let name_of = |uri: &str, offset: usize| match st.name_target(uri, offset) {
        Some(Target::Export(_, name)) => Some(name),

        _ => None,
    };

    for (offset, what) in [
        (user.find("Vec2 }").expect("import"), "import"),
        (user.find("Vec2 {").expect("construction"), "construction"),
        (user.find("Vec2):").expect("annotation"), "annotation"),
    ] {
        assert_eq!(name_of(&uris[1], offset).as_deref(), Some("Vec2"), "{what}");
    }

    assert_eq!(
        name_of(&uris[0], module.find("Vec2").expect("declaration")).as_deref(),
        Some("Vec2")
    );

    // The declaration, the `impl`-free module, the import, the
    // construction, and the annotation: four places in two files.
    let edit = st
        .export_rename(&dir.join("src").join("m2.aly"), "Vec2", "Zed")
        .expect("rename");
    let count: usize = edit["changes"]
        .as_object()
        .expect("changes")
        .values()
        .map(|v| v.as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(count, 4, "{edit}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An enum variant: the caret in the enum body and the caret on
/// `Shape.Circle` in another file both name the variant, and the edit
/// reaches the declaration and the use.
#[test]
pub(crate) fn a_variant_renames_where_it_is_declared_and_used() {
    let module = "export enum Shape as\n    Circle(number)\n    Square(number)\nend\n";
    let user = "import { Shape } from \"./m3\"\nlocal c = Shape.Circle(3)\nprint(c)\n";
    let dir = std::env::temp_dir().join(format!("alloy-nav-variant-{}", std::process::id()));
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

    for (rel, src) in [("m3.aly", module), ("u3.aly", user)] {
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

    let variant = |uri: &str, offset: usize| match st.name_target(uri, offset) {
        Some(Target::Variant { owner, name, .. }) => Some((owner, name)),

        _ => None,
    };
    let wanted = Some(("Shape".to_string(), "Circle".to_string()));
    assert_eq!(
        variant(&uris[0], module.find("Circle").expect("declaration")),
        wanted
    );
    assert_eq!(variant(&uris[1], user.find("Circle").expect("use")), wanted);

    // A word no enum declares keeps to the other answers.
    assert_eq!(variant(&uris[1], user.find("Shape").expect("Shape")), None);

    let edit = st
        .variant_edits(&dir.join("src").join("m3.aly"), "Shape", "Circle", "Round")
        .expect("edit");
    let count: usize = edit["changes"]
        .as_object()
        .expect("changes")
        .values()
        .map(|v| v.as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(count, 2, "{edit}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A variant's rename and its reference list reach the `case` patterns
/// too: bare, dotted, a unit variant alone, one nested in a payload, and
/// one inside a struct pattern. A local of the same name is no use.
#[test]
pub(crate) fn a_variant_rename_reaches_the_match_patterns() {
    let module = "export enum Opt as\n    Some(number),\n    Nil\nend\n\nexport enum Box as\n    Hold(Opt)\nend\n";
    let user = concat!(
        "import { Opt, Box } from \"./m4\"\n",
        "local function f(o: Opt, b: Box)\n",
        "    match o with\n",
        "        case Some(v) then print(v)\n",
        "        case Opt.Some(v) then print(v)\n",
        "        case Nil then print(0)\n",
        "        default print(1)\n",
        "    end\n",
        "    match b with\n",
        "        case Hold(Some(n)) then print(n)\n",
        "        case Hold(Nil) then print(0)\n",
        "    end\n",
        "    match { kind = o } with\n",
        "        case { kind = Some(x) } then print(x)\n",
        "        case { kind = Nil } then print(0)\n",
        "    end\n",
        "    local Some = 1\n",
        "    print(Some)\n",
        "end\n",
    );
    let dir = std::env::temp_dir().join(format!("alloy-nav-pattern-{}", std::process::id()));
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

    for (rel, src) in [("m4.aly", module), ("u4.aly", user)] {
        let path = dir.join("src").join(rel);
        std::fs::write(&path, src).expect(rel);
        let options = EmitOptions {
            file_name: path.to_string_lossy().into_owned(),
            ..EmitOptions::default()
        };
        st.docs.insert(
            format!("file://{}", path.display()),
            Doc::new(
                src.to_string(),
                1,
                &options,
                &alloy::luaux::Config::default(),
                None,
            ),
        );
    }

    let module_path = dir.join("src").join("m4.aly");
    let user_uri = format!("file://{}", dir.join("src").join("u4.aly").display());
    let edits_of = |name: &str| {
        let edit = st
            .variant_edits(&module_path, "Opt", name, "Other")
            .expect("edit");
        let count = |uri: &str| edit["changes"][uri].as_array().map_or(0, Vec::len);
        let module_uri = format!("file://{}", module_path.display());

        (count(&module_uri), count(&user_uri))
    };

    // The declaration; then `Some(v)`, `Opt.Some(v)`, `Hold(Some(n))`,
    // and `{ kind = Some(x) }`. The local named `Some` stays.
    assert_eq!(edits_of("Some"), (1, 4));
    // The declaration; then `case Nil`, `Hold(Nil)`, and
    // `{ kind = Nil }`.
    assert_eq!(edits_of("Nil"), (1, 3));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A struct field's rename reaches the constructor key and the field's
/// own declaration line, and leaves another struct's key of that name
/// alone.
///
/// The emit hands the key table to a constructor, where the child reads
/// a plain record; the field list is generated text, so the child's edit
/// of the declaration lands on the byte the header came from.
#[test]
fn a_field_rename_reaches_the_constructor_and_the_declaration() {
    const SRC: &str = concat!(
        "struct Point as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "struct Other as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "local p = new Point { x = 1 }\n",
        "local o = new Other { x = 2 }\n",
        "\n",
        "local function read(c: Point): number\n",
        "    return c.x\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    // What the child answers: the read of `c.x`, and a declaration edit
    // that mapped onto the `end` of the struct.
    let mut result = json!({
        "changes": {
            uri: [
                { "range": range_value((12, 13), (12, 14)), "newText": "zz" },
                { "range": range_value((2, 0), (2, 1)), "newText": "zz" },
            ],
        },
    });
    st.mend_field_rename(uri, 12, 13, &mut result);

    let edits: Vec<(u64, u64)> = result["changes"][uri]
        .as_array()
        .expect("edits")
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().expect("line"),
                e["range"]["start"]["character"].as_u64().expect("column"),
            )
        })
        .collect();

    // The declaration of the field, the constructor key of `Point`, and
    // the read. `Other`'s key of the same name stays, and so does the
    // `end` the child pointed at.
    assert_eq!(edits, [(1, 4), (8, 22), (12, 13)], "{result}");
}

/// The references answer for a field read gets the same mend: the
/// child's location on the struct's `end` goes, and the declaration
/// and the constructor key join the read.
#[test]
fn field_references_reach_the_constructor_and_the_declaration() {
    const SRC: &str = concat!(
        "struct Point as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "local p = new Point { x = 1 }\n",
        "\n",
        "local function read(c: Point): number\n",
        "    return c.x\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let mut result = json!([
        { "uri": uri, "range": range_value((7, 13), (7, 14)) },
        { "uri": uri, "range": range_value((2, 0), (2, 1)) },
    ]);
    st.mend_field_references(uri, 7, 13, &mut result);

    let mut sites: Vec<(u64, u64)> = result
        .as_array()
        .expect("locations")
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().expect("line"),
                e["range"]["start"]["character"].as_u64().expect("column"),
            )
        })
        .collect();
    sites.sort_unstable();

    assert_eq!(sites, [(1, 4), (4, 22), (7, 13)], "{result}");
}

/// A trait's method is one name in three places: the trait's own
/// declaration, every `impl Trait for S`, and a call on a value of such
/// a struct. A plain `impl` that shares the spelling is another method.
///
/// The emit gives a trait no table and types the receiver as `any`, so
/// the child answers with the impl it stands in and nothing else.
#[test]
fn a_trait_method_renames_the_trait_every_impl_and_the_calls() {
    const TRAIT: &str = concat!(
        "export trait Greet as\n",
        "    function hello(self): string\n",
        "end\n",
        "\n",
        "export struct Alpha as\n",
        "    n: number\n",
        "end\n",
        "\n",
        "impl Greet for Alpha as\n",
        "    function hello(self): string\n",
        "        return \"alpha\"\n",
        "    end\n",
        "end\n",
    );
    // `Loud` meets no trait, so its `hello` is a method of its own.
    const USER: &str = concat!(
        "import { Greet } from \"./tr\"\n",
        "\n",
        "export struct Beta as\n",
        "    n: number\n",
        "end\n",
        "\n",
        "impl Greet for Beta as\n",
        "    function hello(self): string\n",
        "        return \"beta\"\n",
        "    end\n",
        "end\n",
        "\n",
        "struct Loud as\n",
        "    n: number\n",
        "end\n",
        "\n",
        "impl Loud as\n",
        "    function hello(self): string\n",
        "        return \"loud\"\n",
        "    end\n",
        "end\n",
        "\n",
        "local b = new Beta { n = 1 }\n",
        "local l = new Loud { n = 2 }\n",
        "print(b:hello(), l:hello())\n",
    );
    let st = super::support::files(&[("file:///tr.aly", TRAIT), ("file:///u.aly", USER)]);
    let call = USER.find("b:hello").expect("the call") + 2;
    let target = st.name_target("file:///u.aly", call);

    let Some(Target::Method { trait_name, name }) = target else {
        panic!("the caret names no trait method");
    };

    assert_eq!((trait_name.as_str(), name.as_str()), ("Greet", "hello"));

    let edit = st
        .method_edits(&trait_name, &name, "greetings")
        .expect("edit");
    let at = |uri: &str, src: &str| -> Vec<usize> {
        edit["changes"][uri]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|e| {
                let (line, column) = position_of_value(&e["range"]["start"]).expect("position");
                offset_of(src, line, column).expect("offset")
            })
            .collect()
    };

    // The trait's declaration and `Alpha`'s impl.
    assert_eq!(
        at("file:///tr.aly", TRAIT),
        [
            TRAIT.find("hello").expect("declaration"),
            TRAIT.rfind("hello").expect("the impl"),
        ],
        "{edit}"
    );

    // `Beta`'s impl and the call on `b`. `Loud`'s own method and the
    // call on `l` share the spelling and stay as they are.
    assert_eq!(
        at("file:///u.aly", USER),
        [USER.find("hello").expect("the impl"), call],
        "{edit}"
    );
}

/// A receiver typed by the trait reaches the method with no impl in
/// between: a parameter bound `<T: Speaker>`, an annotation `s: Speaker`,
/// a bound of two traits, and `self` in the trait's own default body. A
/// call on a namespace the file never binds shares the spelling and
/// stays as it is.
#[test]
fn a_receiver_typed_by_the_trait_is_a_site_of_its_method() {
    const SRC: &str = concat!(
        "trait Speaker as\n",
        "    function speak(self): string\n",
        "    function twice(self): string\n",
        "        return self.speak() .. self.speak()\n",
        "    end\n",
        "end\n",
        "\n",
        "trait Loud as\n",
        "    function shout(self): string\n",
        "end\n",
        "\n",
        "function announce<T: Speaker>(s: T): string\n",
        "    return s.speak()\n",
        "end\n",
        "\n",
        "function direct(s: Speaker): string\n",
        "    return s:speak()\n",
        "end\n",
        "\n",
        "function both<U: Loud & Speaker>(s: U): string\n",
        "    return s.speak()\n",
        "end\n",
        "\n",
        "print(Log.speak(\"x\"))\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let sites: Vec<usize> = SRC.match_indices("speak(").map(|(i, _)| i).collect();
    // The declaration, the two `self` calls, the bound, the annotation,
    // and the two-trait bound: `Log.speak` is not one of them.
    let wanted = &sites[..6];
    assert_eq!(sites.len(), 7);

    for at in [wanted[0], wanted[3], wanted[4]] {
        let Some(Target::Method { trait_name, name }) = st.name_target(uri, at) else {
            panic!("no trait method at {at}");
        };
        assert_eq!((trait_name.as_str(), name.as_str()), ("Speaker", "speak"));
    }

    let edit = st.method_edits("Speaker", "speak", "talk").expect("edit");
    let got: Vec<usize> = edit["changes"][uri]
        .as_array()
        .expect("edits")
        .iter()
        .map(|e| {
            let (line, column) = position_of_value(&e["range"]["start"]).expect("position");
            offset_of(SRC, line, column).expect("offset")
        })
        .collect();
    assert_eq!(got, wanted, "{edit}");
}

/// A definitions file names a type with no import of it, so the rename
/// walk, which reads the import lists, left it behind. An ambient
/// declaration stands in scope everywhere, and only a type reaches one:
/// a renamed value leaves those files alone.
#[test]
fn a_type_rename_reaches_a_definitions_file() {
    const MODULE: &str = concat!(
        "export enum Kind as\n",
        "    Good\n",
        "end\n",
        "\n",
        "export const limit = 3\n",
    );
    const AMBIENT: &str = concat!(
        "declare function useKind(v: Kind): ()\n",
        "declare limit: number\n",
    );
    let st = super::support::files(&[("file:///m.aly", MODULE), ("file:///a.d.aly", AMBIENT)]);
    let file = PathBuf::from("/m.aly");
    let edit = st.export_rename(&file, "Kind", "Sort").expect("edit");
    let at = |edit: &Value, uri: &str| -> Vec<u64> {
        edit["changes"][uri]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|e| e["range"]["start"]["character"].as_u64().expect("column"))
            .collect()
    };

    assert_eq!(
        at(&edit, "file:///a.d.aly"),
        [AMBIENT.rfind("Kind").expect("the use") as u64],
        "{edit}"
    );

    // `limit` is a value. An ambient file cannot read one from a
    // module, so its own `limit` is another name.
    let edit = st.export_rename(&file, "limit", "cap").expect("edit");
    assert!(at(&edit, "file:///a.d.aly").is_empty(), "{edit}");
}

/// A field of a struct a namespace holds. The declaration answers to
/// its path and to the name the emit gives it, and neither is the word
/// the source writes in front of the body.
#[test]
fn a_field_of_a_namespace_member_names_the_member() {
    const SRC: &str = concat!(
        "namespace Ns as
",
        "    struct T as
",
        "        amount: number,
",
        "    end
",
        "end
",
        "
",
        "local t = new Ns.T { amount = 1 }
",
        "print(t.amount)
",
    );
    let (st, uri) = super::support::one_file(SRC);
    let at = SRC.find("amount: number").expect("the field");
    let Some(Target::Field { owner, name }) = st.name_target(uri, at) else {
        panic!("the field names no target");
    };

    assert_eq!((owner.as_str(), name.as_str()), ("T", "amount"));

    // The declaration, the constructor key, and the read.
    assert_eq!(
        rows(
            &st.field_edits("T", "amount", "qty")
                .expect("the field edits")
        ),
        [
            "t.aly 2:8-14 -> qty",
            "t.aly 6:21-27 -> qty",
            "t.aly 7:8-14 -> qty"
        ]
    );
}

/// A caret in a struct body names the field, and a caret on a struct
/// the file keeps to itself names the struct. The emit rewrites the
/// field list and a declaration with no export, so the child finds no
/// symbol at either place.
#[test]
fn a_declaration_names_a_field_and_a_local_struct() {
    const SRC: &str = concat!(
        "struct Widget as\n",
        "    x: number\n",
        "end\n",
        "\n",
        "local w = new Widget { x = 1 }\n",
        "local function read(v: Widget): number\n",
        "    return v.x\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let at = SRC.find("x: number").expect("the field");
    let Some(Target::Field { owner, name }) = st.name_target(uri, at) else {
        panic!("the field names no target");
    };

    assert_eq!((owner.as_str(), name.as_str()), ("Widget", "x"));

    // The declaration, the constructor key, and the read.
    assert_eq!(
        rows(
            &st.field_edits("Widget", "x", "zz")
                .expect("the field edits")
        ),
        [
            "t.aly 1:4-5 -> zz",
            "t.aly 4:23-24 -> zz",
            "t.aly 6:13-14 -> zz"
        ]
    );

    // The struct: its own line, the constructor, and the annotation.
    for text in ["Widget as", "Widget {", "Widget)"] {
        let at = SRC.find(text).expect(text);

        assert!(
            matches!(st.name_target(uri, at), Some(Target::Local(ref n)) if n == "Widget"),
            "{text}"
        );
    }
}

/// The caret on an alias reads the entry the alias belongs to, wherever
/// that entry sits in the list. A later entry is another name and
/// another export, and the alias is this file's own word.
#[test]
fn an_alias_beside_another_entry_reads_its_own_entry() {
    for src in [
        "import { version as ver, make } from \"./m\"\nprint(ver, make)\n",
        "import { make, version as ver } from \"./m\"\nprint(ver, make)\n",
    ] {
        let (st, uri) = super::support::one_file(src);
        let entry = import_entries(src)
            .into_iter()
            .find(|e| e.bound == "ver")
            .expect("the alias entry");
        let (start, end) = entry.alias_at.expect("the alias range");

        assert_eq!(&src[start..end], "ver");
        assert_eq!(entry.name, "version");

        // The caret on the alias, and the caret right behind it.
        for offset in [start, end] {
            let found = st.import_entry_at(src, offset).expect("an entry");

            assert_eq!(found.name, "version", "{src} at {offset}");
            assert!(
                matches!(st.name_target(uri, offset), Some(Target::Local(ref n)) if n == "ver"),
                "{src} at {offset}"
            );
        }
    }
}

/// Every declaration kind the emit rewrites answers from the source.
/// The child maps its edits for one back onto the byte the header came
/// from: the `e` of `enum`, the `t` of `trait`, a letter of a variant.
#[test]
fn a_local_declaration_of_any_kind_names_a_target() {
    const SRC: &str = concat!(
        "enum Suit as\n",
        "    Hearts\n",
        "end\n",
        "\n",
        "trait Flyer as\n",
        "    function fly(self): string\n",
        "end\n",
        "\n",
        "interface Sized as\n",
        "    size: number\n",
        "end\n",
        "\n",
        "type Pair = { a: number }\n",
        "\n",
        "attribute tagged(name: string) on struct\n",
        "\n",
        "macro twice(n)\n",
        "    n + n\n",
        "end\n",
        "\n",
        "namespace Geo as\n",
        "    const PI = 3\n",
        "end\n",
        "\n",
        "local suit = Suit.Hearts\n",
    );
    let (st, uri) = super::support::one_file(SRC);

    for (text, name) in [
        ("Suit as", "Suit"),
        ("Flyer as", "Flyer"),
        ("Sized as", "Sized"),
        ("Pair =", "Pair"),
        ("tagged(", "tagged"),
        ("twice(", "twice"),
        ("Geo as", "Geo"),
    ] {
        let at = SRC.find(text).expect(text);

        assert!(
            matches!(st.name_target(uri, at), Some(Target::Local(ref n)) if n == name),
            "{text}"
        );
    }

    // The enum and its use, and nothing of the generated text between.
    let at = SRC.find("Suit as").expect("the enum");
    let Some(Target::Local(name)) = st.name_target(uri, at) else {
        panic!("the enum names no target");
    };

    assert_eq!(
        name_uses(SRC, &name)
            .into_iter()
            .map(|(s, _)| position_of(SRC, s).0 + 1)
            .collect::<Vec<u32>>(),
        [1, 25]
    );
}

/// The caret's own binding says what a rename touches. One file writes
/// `format` as a struct, as a parameter, as a `local`, and as a `for`
/// variable; the child holds the scopes of the last three, so the proxy
/// answers for the struct alone.
#[test]
fn a_value_binding_at_the_caret_stays_with_the_child() {
    const SRC: &str = concat!(
        "struct format as\n",
        "    v: number\n",
        "end\n",
        "\n",
        "function useit(format: string): string\n",
        "    return format\n",
        "end\n",
        "\n",
        "function loopit()\n",
        "    local format = 1\n",
        "    for format = 1, 3 do\n",
        "        print(format)\n",
        "    end\n",
        "    print(format)\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let at = |text: &str| SRC.find(text).expect(text);

    // The struct's name is generated text after the emit, so the proxy
    // answers with the file's own uses of it.
    assert!(
        matches!(st.name_target(uri, at("format as")), Some(Target::Local(ref n)) if n == "format")
    );

    for text in [
        "format: string",
        "format\nend",
        "format = 1\n",
        "format = 1,",
        "format)\n    end",
    ] {
        assert!(st.name_target(uri, at(text)).is_none(), "{text}");
    }
}

/// A field of a struct and a method of its own `impl` land on one table
/// after the emit, so either name refuses a rename onto the other. The
/// child reads the artifact, where the field list is generated text.
#[test]
fn a_field_and_an_impl_method_refuse_each_other() {
    const SRC: &str = concat!(
        "struct Box as\n",
        "    size: number\n",
        "end\n",
        "\n",
        "impl Box as\n",
        "    function area(self): number\n",
        "        return self.size * self.size\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let at = |text: &str| SRC.find(text).expect(text);
    let field = st.name_target(uri, at("size: number"));

    assert!(matches!(field, Some(Target::Field { .. })));
    assert_eq!(
        st.rename_clash(uri, at("size: number"), field.as_ref(), "area")
            .as_deref(),
        Some("`area` is already a method on line 6")
    );

    // The other way round. The method is the struct's own, and the
    // fields it stands beside are the proxy's to read.
    let method = at("area(self)");
    let target = st.name_target(uri, method);

    assert!(matches!(&target, Some(Target::Method { trait_name, .. }) if trait_name == "Box"));
    assert_eq!(
        st.rename_clash(uri, method, target.as_ref(), "size")
            .as_deref(),
        Some("`size` is already a field on line 2")
    );
}

/// A rename onto a name the scope already binds is refused. The edit
/// set would bind one name twice, and the reader would lose what the
/// lines below it mean.
#[test]
fn a_rename_onto_a_bound_name_is_refused() {
    const SRC: &str = concat!(
        "local a = 1\n",
        "local b = 2\n",
        "local _c = a + b\n",
        "\n",
        "export struct Point as\n",
        "    x: number\n",
        "    y: number\n",
        "end\n",
        "\n",
        "export function foo(): number\n",
        "    return 1\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let at = |text: &str| SRC.find(text).expect(text);

    // The child renames a local, and the clash is still the reader's.
    assert_eq!(
        st.rename_clash(uri, at("a = 1"), None, "b").as_deref(),
        Some("`b` is already a local on line 2")
    );
    assert_eq!(st.rename_clash(uri, at("a = 1"), None, "z"), None);

    // A field of the same struct.
    let field = st.name_target(uri, at("x: number"));

    assert_eq!(
        st.rename_clash(uri, at("x: number"), field.as_ref(), "y")
            .as_deref(),
        Some("`y` is already a field on line 7")
    );

    // An export, against a name its own module declares.
    let export = st.name_target(uri, at("foo("));

    assert!(matches!(export, Some(Target::Export(..))));
    assert_eq!(
        st.rename_clash(uri, at("foo("), export.as_ref(), "Point")
            .as_deref(),
        Some("`Point` is already a struct on line 5")
    );
}

/// A `case` binding belongs to the proxy. The match lowers to one
/// expression, so the child's edits land on generated text: the `case`
/// keyword and the `end` of the enum. The arm is the whole scope, so a
/// binding of one arm never reads a use of another.
#[test]
fn a_case_binding_answers_from_its_own_arm() {
    const SRC: &str = concat!(
        "enum Shape as\n",
        "    Circle(number),\n",
        "    Rect(number),\n",
        "end\n",
        "\n",
        "struct Point as\n",
        "    x: number,\n",
        "end\n",
        "\n",
        "local function area(s: Shape, p: Point): number\n",
        "    match s with\n",
        "        case Circle(r) then return r * r\n",
        "        case Rect(r) then\n",
        "            match p with\n",
        "                case Point { x } then return x + r\n",
        "            end\n",
        "    end\n",
        "\n",
        "    return 0\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    // The rename and the reference list read the uses of the arm, each
    // as a line and a column.
    let uses = |text: &str| {
        let at = SRC.find(text).expect(text);

        match st.name_target(uri, at) {
            Some(Target::Binding { name, start, end }) => uses_in_range(SRC, &name, start, end)
                .into_iter()
                .map(|(s, _)| position_of(SRC, s))
                .collect::<Vec<(u32, u32)>>(),

            _ => panic!("{text} names no arm binding"),
        }
    };

    // The payload binding of the first arm, from its pattern and from a
    // use of it. The `r` of the arm below is another name.
    assert_eq!(
        uses("r) then return"),
        [(11, 20), (11, 35), (11, 39)],
        "the payload binding"
    );
    assert_eq!(uses("r * r"), [(11, 20), (11, 35), (11, 39)], "a use of it");

    // The shorthand of a struct pattern, in an arm of a nested match.
    assert_eq!(uses("x } then"), [(14, 29), (14, 45)], "the shorthand");

    // The arm of the outer match reaches over the nested one, so the
    // binding the inner arm reads is still the outer arm's.
    assert_eq!(
        uses("r) then\n"),
        [(12, 18), (14, 49)],
        "the binding of the outer arm"
    );
    assert_eq!(uses("r\n            end"), [(12, 18), (14, 49)], "its use");

    // The parameters keep their scopes with the child.
    for text in ["s: Shape", "p: Point", "s with", "p with"] {
        let at = SRC.find(text).expect(text);

        assert!(st.name_target(uri, at).is_none(), "{text}");
    }
}

/// A member of an exported namespace, under every word a reader puts
/// in front of it: the module's own `Ns.T`, the `Ns` an import list
/// binds plain, the alias of `import { Ns as A }`, and the `M.Ns` of a
/// module binding. The emit flattens the group, so the child ties none
/// of the four to the declaration.
#[test]
pub(crate) fn a_rename_of_a_namespace_member_reaches_every_head() {
    let dir = std::env::temp_dir().join(format!("alloy-nav-ns-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");

    let sources = [
        (
            "ns.aly",
            "export namespace Ns as\n    struct T as\n        value: number,\n    end\nend\n\nlocal here = new Ns.T { value = 0 }\nprint(here)\n",
        ),
        (
            "alias.aly",
            "import { Ns as A } from \"./ns\"\n\nlocal a = new A.T { value = 1 }\nprint(a)\n",
        ),
        (
            "plain.aly",
            "import { Ns } from \"./ns\"\n\nlocal b = new Ns.T { value = 2 }\nprint(b)\n",
        ),
        (
            "star.aly",
            "import * as M from \"./ns\"\n\nlocal c = new M.Ns.T { value = 3 }\nprint(c)\n",
        ),
    ];
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        snippets: true,
        ..State::default()
    };

    for (rel, src) in sources {
        let path = dir.join("src").join(rel);
        std::fs::write(&path, src).expect(rel);
        let options = EmitOptions {
            file_name: path.to_string_lossy().into_owned(),
            ..EmitOptions::default()
        };
        st.docs.insert(
            format!("file://{}", path.display()),
            Doc::new(
                src.to_string(),
                1,
                &options,
                &alloy::luaux::Config::default(),
                None,
            ),
        );
    }

    let file = dir.join("src").join("ns.aly");
    let edit = st.export_rename(&file, "T", "Item").expect("the member");

    assert_eq!(
        rows(&edit),
        [
            "alias.aly 2:16-17 -> Item",
            "ns.aly 1:11-12 -> Item",
            "ns.aly 6:20-21 -> Item",
            "plain.aly 2:17-18 -> Item",
            "star.aly 2:19-20 -> Item",
        ]
    );

    // A use under an alias names the member, so the rename that starts
    // there writes the same edits.
    let uri = format!("file://{}", dir.join("src").join("alias.aly").display());
    let source = &st.docs[&uri].source;
    let at = source.find("A.T").expect("the use") + 2;

    assert!(
        matches!(st.name_target(&uri, at), Some(Target::Export(ref f, ref n)) if *f == file && n == "T")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The group of a namespace reads under every word a file writes it
/// under. `M.Ns` stands after a dot, so the walk over plain names never
/// saw it, and references on the group answered with the declaration
/// alone.
#[test]
pub(crate) fn a_namespace_group_reads_under_every_head() {
    let dir = std::env::temp_dir().join(format!("alloy-nav-group-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");

    let sources = [
        (
            "group.aly",
            "export namespace Ns as\n    struct T as\n        value: number,\n    end\nend\n",
        ),
        (
            "gstar.aly",
            "import * as M from \"./group\"\n\nlocal c = new M.Ns.T { value = 3 }\nprint(c)\n",
        ),
        (
            "galias.aly",
            "import { Ns as A } from \"./group\"\n\nlocal a = new A.T { value = 1 }\nprint(a)\n",
        ),
    ];
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        snippets: true,
        ..State::default()
    };

    for (rel, src) in sources {
        let path = dir.join("src").join(rel);
        std::fs::write(&path, src).expect(rel);
        let options = EmitOptions {
            file_name: path.to_string_lossy().into_owned(),
            ..EmitOptions::default()
        };
        st.docs.insert(
            format!("file://{}", path.display()),
            Doc::new(
                src.to_string(),
                1,
                &options,
                &alloy::luaux::Config::default(),
                None,
            ),
        );
    }

    let module = imports::module_path(&dir.join("src").join("group.aly"));
    let sites = |rel: &str| {
        let uri = format!("file://{}", dir.join("src").join(rel).display());
        let source = st.docs[&uri].source.clone();

        st.group_uses(&uri, &source, &module, "Ns")
            .into_iter()
            .map(|(s, _)| {
                let (line, character) = position_of(&source, s);

                format!("{rel} {}:{character}", line + 1)
            })
            .collect::<Vec<String>>()
    };

    assert_eq!(sites("group.aly"), ["group.aly 1:17"]);
    assert_eq!(sites("gstar.aly"), ["gstar.aly 3:16"]);
    // The list binds the alias, so the entry and the alias are
    // both this group.
    assert_eq!(
        sites("galias.aly"),
        ["galias.aly 1:9", "galias.aly 1:15", "galias.aly 3:14"]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Go to definition on the call after `try` answered nothing: the
/// desugar wraps the call, so the child sees generated text there. The
/// plain call under it lands on the declaration, and so does this one.
#[test]
fn a_call_the_desugar_moved_still_finds_its_declaration() {
    let src = "export function safe_div(a: number, b: number): Result<number, string>\n    if b == 0 then\n        return Err(\"nope\")\n    end\n    return Ok(a / b)\nend\n\nexport function caller(a: number, b: number): Result<number, string>\n    local x = try safe_div(a, b)\n    local y = safe_div(a, b)\n    return Ok(x)\nend\n";
    let (st, uri) = super::support::one_file(src);
    let declaration = json!([{
        "uri": uri,
        "range": { "start": { "line": 0, "character": 16 }, "end": { "line": 0, "character": 24 } },
    }]);

    // The `try` site, then the plain call.
    assert_eq!(
        st.declared_definition(uri, 8, 22),
        Some(declaration.clone())
    );
    assert_eq!(st.declared_definition(uri, 9, 21), Some(declaration));

    // The argument on the `try` line is the parameter of `caller`.
    assert_eq!(
        st.declared_definition(uri, 8, 27),
        Some(json!([{
            "uri": uri,
            "range": { "start": { "line": 7, "character": 23 }, "end": { "line": 7, "character": 24 } },
        }]))
    );
    assert_eq!(st.declared_definition(uri, 8, 16), None);
}

/// A function called above its declaration gets a forward `local` on
/// line 1 of the emit, as generated text anchored at the first byte of
/// the file. The child lists that site with the real ones; the proxy
/// drops it from a references list, a rename, and a definition, and
/// the definition then lands on the declaration.
#[test]
fn a_hoisted_function_keeps_its_forward_declaration_out_of_sight() {
    const SRC: &str = concat!(
        "function fact(n: number): number\n",
        "    return n\n",
        "end\n",
        "\n",
        "function isEven(n: number): boolean\n",
        "    return isOdd(n - 1)\n",
        "end\n",
        "\n",
        "function isOdd(n: number): boolean\n",
        "    return isEven(n - 1)\n",
        "end\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let anchor = range_value((0, 0), (0, 1));
    let call = range_value((5, 11), (5, 16));
    let declaration = range_value((8, 9), (8, 14));

    let mut refs = json!([
        { "uri": uri, "range": anchor },
        { "uri": uri, "range": call },
        { "uri": uri, "range": declaration },
    ]);
    st.drop_stray_sites(uri, 8, 9, &mut refs);
    assert_eq!(
        refs,
        json!([{ "uri": uri, "range": call }, { "uri": uri, "range": declaration }])
    );

    let mut rename = json!({ "changes": { uri: [
        { "range": anchor, "newText": "odd" },
        { "range": call, "newText": "odd" },
        { "range": declaration, "newText": "odd" },
    ] } });
    st.drop_stray_sites(uri, 8, 9, &mut rename);
    assert_eq!(
        rename,
        json!({ "changes": { uri: [
            { "range": call, "newText": "odd" },
            { "range": declaration, "newText": "odd" },
        ] } })
    );

    // The child's definition is the forward `local`: gone, and the
    // declaration answers from the call and from its own name.
    let mut definition = json!([{ "uri": uri, "range": anchor }]);
    st.drop_stray_sites(uri, 5, 11, &mut definition);
    assert_eq!(definition, json!([]));
    let found = Some(json!([{ "uri": uri, "range": declaration }]));
    assert_eq!(st.declared_definition(uri, 5, 11), found);
    assert_eq!(st.declared_definition(uri, 8, 9), found);
}

/// A method of a plain `impl` reaches its declaration and every call
/// from either end: a `:` call, a `Counter.bump(c)` call, a private
/// method, and a call in a file that imports the struct. The child
/// finds no site through a `:` call on the struct's table.
#[test]
fn a_struct_method_renames_the_declaration_and_every_call() {
    const COUNTER: &str = concat!(
        "export struct Counter as\n",
        "    count: number = 0\n",
        "end\n",
        "\n",
        "impl Counter as\n",
        "    function bump(self)\n",
        "        self.count += 1\n",
        "    end\n",
        "\n",
        "    private function reset(self)\n",
        "        self.count = 0\n",
        "    end\n",
        "end\n",
        "\n",
        "function main()\n",
        "    local c = new Counter {}\n",
        "    c:bump()\n",
        "    Counter.bump(c)\n",
        "    c:reset()\n",
        "    local d = Counter {}\n",
        "    d:bump()\n",
        "end\n",
    );
    const USER: &str = concat!(
        "import { Counter } from \"./counter\"\n",
        "\n",
        "local c = new Counter {}\n",
        "c:bump()\n",
    );
    let st = super::support::files(&[("file:///counter.aly", COUNTER), ("file:///user.aly", USER)]);
    let at = |src: &str, text: &str| src.find(text).expect(text);
    let sites = |edit: &Value, uri: &str, src: &str| -> Vec<usize> {
        edit["changes"][uri]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|e| {
                let (line, column) = position_of_value(&e["range"]["start"]).expect("position");
                offset_of(src, line, column).expect("offset")
            })
            .collect()
    };

    for caret in [
        at(COUNTER, "bump(self)"),
        at(COUNTER, "c:bump") + 2,
        at(COUNTER, "Counter.bump") + 8,
        at(USER, "c:bump") + 2,
    ] {
        let uri = match caret == at(USER, "c:bump") + 2 {
            true => "file:///user.aly",

            false => "file:///counter.aly",
        };
        let target = st.name_target(uri, caret);

        assert!(
            matches!(&target, Some(Target::Method { trait_name, name }) if trait_name == "Counter" && name == "bump"),
            "{caret}: {target:?}"
        );
    }

    let edit = st.method_edits("Counter", "bump", "poke").expect("edit");

    assert_eq!(
        sites(&edit, "file:///counter.aly", COUNTER),
        [
            at(COUNTER, "bump(self)"),
            at(COUNTER, "c:bump") + 2,
            at(COUNTER, "Counter.bump") + 8,
            // `Counter {}` with no `new` compiles to nothing, and the
            // reader still means the struct.
            at(COUNTER, "d:bump") + 2,
        ],
        "{edit}"
    );
    assert_eq!(
        sites(&edit, "file:///user.aly", USER),
        [at(USER, "c:bump") + 2],
        "{edit}"
    );

    // A private method reads the same way.
    let reset = st.name_target("file:///counter.aly", at(COUNTER, "c:reset") + 2);

    assert!(matches!(&reset, Some(Target::Method { name, .. }) if name == "reset"));
    assert_eq!(
        sites(
            &st.method_edits("Counter", "reset", "clear").expect("edit"),
            "file:///counter.aly",
            COUNTER
        ),
        [at(COUNTER, "reset(self)"), at(COUNTER, "c:reset") + 2]
    );
}

/// A trait's default method with an empty `impl Trait for S`: the
/// struct writes no method of its own, and a call on it still reaches
/// the trait's declaration, from either end.
#[test]
fn a_default_method_reaches_a_struct_with_an_empty_impl() {
    const SRC: &str = concat!(
        "trait Greeter as\n",
        "    function greet(self): string\n",
        "        return \"hi\"\n",
        "    end\n",
        "end\n",
        "\n",
        "struct Person as\n",
        "    name: string\n",
        "end\n",
        "\n",
        "impl Greeter for Person as\n",
        "end\n",
        "\n",
        "local p = new Person { name = \"a\" }\n",
        "print(p:greet())\n",
    );
    let (st, uri) = super::support::one_file(SRC);
    let declaration = SRC.find("greet(self)").expect("declaration");
    let call = SRC.find("p:greet").expect("call") + 2;

    for caret in [declaration, call] {
        let target = st.name_target(uri, caret);

        assert!(
            matches!(&target, Some(Target::Method { trait_name, name }) if trait_name == "Greeter" && name == "greet"),
            "{caret}: {target:?}"
        );
    }

    let edit = st.method_edits("Greeter", "greet", "hello").expect("edit");
    let starts: Vec<usize> = edit["changes"][uri]
        .as_array()
        .expect("edits")
        .iter()
        .map(|e| {
            let (line, column) = position_of_value(&e["range"]["start"]).expect("position");
            offset_of(SRC, line, column).expect("offset")
        })
        .collect();

    assert_eq!(starts, [declaration, call], "{edit}");
}

/// A component tag names the function: `<Header />` and
/// `<Footer>...</Footer>` are sites of `Header` and `Footer`, from the
/// tag and from the declaration. The lowering writes the call as
/// generated text, so the child renames nothing from a tag.
#[test]
fn a_component_tag_is_a_site_of_its_function() {
    const APP: &str = concat!(
        "export function App()\n",
        "    return (\n",
        "        <Frame>\n",
        "            <Header title=\"Shop\" />\n",
        "            <Footer>\n",
        "                <TextLabel />\n",
        "            </Footer>\n",
        "        </Frame>\n",
        "    )\n",
        "end\n",
        "\n",
        "export function Header(props: { title: string })\n",
        "    return <TextLabel Text={props.title} />\n",
        "end\n",
        "\n",
        "function Footer(props: { children: any })\n",
        "    return <Frame />\n",
        "end\n",
    );
    let st = super::support::files(&[("file:///app.alx", APP)]);
    let uri = "file:///app.alx";
    let at = |text: &str| APP.find(text).expect(text);
    let starts = |edit: &Value, u: &str, src: &str| -> Vec<usize> {
        edit["changes"][u]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|e| {
                let (line, column) = position_of_value(&e["range"]["start"]).expect("position");
                offset_of(src, line, column).expect("offset")
            })
            .collect()
    };

    // From the tag and from the declaration: one target.
    for caret in [at("<Header") + 2, at("function Header") + 10] {
        let target = st.name_target(uri, caret);

        assert!(
            matches!(&target, Some(Target::Export(_, name)) if name == "Header"),
            "{caret}: {target:?}"
        );
    }

    let edit = st
        .export_rename(Path::new("/app.alx"), "Header", "Top")
        .expect("edit");

    assert_eq!(
        starts(&edit, uri, APP),
        [at("<Header") + 1, at("function Header") + 9],
        "{edit}"
    );

    // A function the file keeps to itself: an open tag and a close tag.
    for caret in [at("<Footer>") + 3, at("</Footer>") + 4] {
        let target = st.name_target(uri, caret);

        assert!(
            matches!(&target, Some(Target::Local(name)) if name == "Footer"),
            "{caret}: {target:?}"
        );
    }

    let uses: Vec<usize> = name_uses(APP, "Footer")
        .into_iter()
        .map(|(s, _)| s)
        .collect();

    assert_eq!(
        uses,
        [
            at("<Footer>") + 1,
            at("</Footer>") + 2,
            at("function Footer") + 9
        ]
    );
}
