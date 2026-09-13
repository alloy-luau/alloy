//! An imported name in the editor: where it is declared, and what a
//! rename of it writes.
//!
//! The emit binds a name from an import list in generated text, so the
//! child answers about a byte no author wrote. Its ranges came back one
//! character wide, in the module as often as the using file.

use super::super::navigation::{
    Target, export_span, impl_method_span, import_entries, module_bindings, module_head_line,
    name_uses, trait_method_span,
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
            in_project: true,
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
            in_project: true,
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
            in_project: true,
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
