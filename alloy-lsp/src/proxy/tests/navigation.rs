//! An imported name in the editor: where it is declared, and what a
//! rename of it writes.
//!
//! The emit binds a name from an import list in generated text, so the
//! child answers about a byte no author wrote. Its ranges came back one
//! character wide, in the module as often as the using file.

use super::super::navigation::{export_span, import_entries, module_bindings};
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
