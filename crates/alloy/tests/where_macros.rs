//! `$matches` and user macros in a `where` run against the bound value.
//! The macro text compiles on its own, so the names in it must already
//! be the temps of the condition; before the fix, the loop below read
//! `x` before it was bound and never ran.

use std::fs;
use std::process::Command;

#[test]
fn a_macro_in_where_reads_the_bound_value() {
    let dir = std::env::temp_dir().join(format!("alloy-where-macros-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        r#"local items: Result<number, string>[] = [Ok(1), Ok(2), Ok(3), Err("stop")]
local i = 0
local function next_item(): Result<number, string>?
    i += 1
    return items[i]
end

macro ok(v) $matches(v, Ok(_)) end

local runs = 0
while const x = next_item() where $matches(x, Ok(_)) do
    runs += 1
end

i = 0
local by_macro = 0
while local x = next_item() where $ok(x) do
    by_macro += 1
end

i = 0
local chain = false
if const a = next_item() where $ok(a); const b = next_item() where $matches(b, Ok(_)) then
    chain = true
end
print(runs, by_macro, chain)
"#,
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    // Without `luau` on PATH, the golden case `where_matches` still pins
    // the output.
    if let Ok(run) = Command::new("luau")
        .arg("main.luau")
        .current_dir(dir.join("build"))
        .output()
    {
        let out = String::from_utf8_lossy(&run.stdout).into_owned()
            + &String::from_utf8_lossy(&run.stderr);
        assert_eq!(out.trim(), "3\t3\ttrue", "{out}");
    }

    let _ = fs::remove_dir_all(&dir);
}
