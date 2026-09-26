//! A barrel passes names on from another module: `export { a } from`,
//! or an import and an `export { a }` list. The importer of the barrel
//! gets what an import of the module itself gets.

use std::fs;
use std::process::Command;

const SHAPES: &str = "export enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\n\nexport macro sq(x) (x) * (x) end\n\nexport namespace Geo as\n    struct Vec as\n        x: number\n        y: number\n    end\n    function len(v: Vec): number\n        return math.sqrt(v.x * v.x + v.y * v.y)\n    end\nend\n";

const MAIN: &str = "import { Shape, sq, Geo } from \"./index\"\nimport { Shape as S2, sq as sq2, G } from \"./index2\"\nimport Player from \"./player\"\nimport Mode from \"./mode\"\n\nlocal function area(s: Shape): number\n    return match s with\n        case Shape.Circle(r) then r\n        case Shape.Rect(w, h) then w * h\n    end\nend\nlocal function side(s: S2): number\n    return match s with\n        case S2.Circle(r) then r\n        case S2.Rect(w, _) then w\n    end\nend\nlocal function name(m: Mode): string\n    return match m with\n        case Mode.A then \"a\"\n        case Mode.B(n) then `b{n}`\n    end\nend\nlocal v: Geo.Vec = new Geo.Vec { x = 3, y = 4 }\nlocal w: G.Vec = new G.Vec { x = 6, y = 8 }\nlocal p: Player = new Player { name = \"x\" }\np:hurt(10)\nprint(area(Shape.Rect(2, 3)), side(S2.Rect(4, 1)), $sq(3), $sq2(4), Geo.len(v), G.len(w), p.hp, name(Mode.B(2)))\n";

#[test]
fn a_barrel_passes_on_enums_macros_namespaces_and_defaults() {
    let dir = std::env::temp_dir().join(format!("alloy-barrels-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    let files = [
        ("shapes", SHAPES),
        ("index", "export { Shape, sq, Geo } from \"./shapes\"\n"),
        (
            "index2",
            "import { Shape, sq, Geo } from \"./shapes\"\nexport { Shape, sq, Geo as G }\n",
        ),
        (
            "player",
            "export default struct Player as\n    name: string\n    hp: number = 100\nend\nimpl Player as\n    function hurt(self, n: number)\n        self.hp -= n\n    end\nend\n",
        ),
        (
            "mode",
            "export default enum Mode as\n    A\n    B(number)\nend\n",
        ),
        ("main", MAIN),
    ];

    for (name, text) in files {
        fs::write(dir.join(format!("src/{name}.aly")), text).unwrap();
    }

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let read = |name: &str| fs::read_to_string(dir.join(format!("build/{name}.luau"))).unwrap();
    let index = read("index");
    let index2 = read("index2");

    // A macro is no value, so no table carries it; the namespace's
    // types go out with it.
    assert!(
        index.contains("export type Geo_Vec = _m1.Geo_Vec"),
        "{index}"
    );
    assert!(!index.contains("sq ="), "{index}");
    assert!(index2.contains("export type G_Vec = Geo_Vec"), "{index2}");
    assert!(!index2.contains("sq ="), "{index2}");
    assert!(read("player").contains("export type Player ="));
    let main = read("main");
    assert!(
        main.contains("local Player = _m3.default type Player = _m3.Player"),
        "{main}"
    );
    // The impl is the struct's own, so the call needs no dispatcher.
    assert!(main.contains("p:hurt(10)"), "{main}");

    let run = Command::new("luau")
        .arg("main.luau")
        .current_dir(dir.join("build"))
        .output();

    if let Ok(run) = run {
        let out = String::from_utf8_lossy(&run.stdout).into_owned()
            + &String::from_utf8_lossy(&run.stderr);
        assert_eq!(out.trim(), "6\t4\t9\t16\t5\t10\t90\tb2", "{out}");
    }

    let _ = fs::remove_dir_all(&dir);
}

/// Luau keeps types and values apart, and Vide exports a value and a
/// type by the name `source`. Its entry module returns what it
/// requires. The barrel and a plain import send both on; a name that is
/// a type alone stays a type.
#[test]
fn a_value_and_a_type_of_one_name_both_pass() {
    let dir = std::env::temp_dir().join(format!("alloy-barrels-both-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src/lib")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    let files = [
        (
            "lib/core.luau",
            "local function source(v) return function() return v end end\nreturn { source = source }\n",
        ),
        (
            "lib/init.luau",
            "local m = require(\"@self/core\")\nexport type source<T> = () -> T\nexport type Only = number\nreturn m\n",
        ),
        ("index.aly", "export { source, Only } from \"./lib\"\n"),
        (
            "main.aly",
            "import { source } from \"./index\"\nimport { source as direct } from \"./lib\"\nprint(source(1)(), direct(2)())\n",
        ),
    ];

    for (name, text) in files {
        fs::write(dir.join("src").join(name), text).unwrap();
    }

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let read = |name: &str| fs::read_to_string(dir.join(format!("build/{name}.luau"))).unwrap();
    let index = read("index");

    assert!(
        index.contains("export type source<T> = _m1.source<T>"),
        "{index}"
    );
    assert!(index.contains("export type Only = _m1.Only"), "{index}");
    assert!(index.contains("return { source = _m1.source }"), "{index}");
    assert!(
        read("main").contains("local direct = _m2.source"),
        "{}",
        read("main")
    );

    let _ = fs::remove_dir_all(&dir);
}
