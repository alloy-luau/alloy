#!/usr/bin/env python3
"""Regenerates crates/alloy/src/roblox_classes.rs and
crates/alloy/src/roblox_props.rs from the vendored globalTypes.d.luau.
Run it after refreshing the definitions file."""
import pathlib, re
here = pathlib.Path(__file__).resolve()
root = next(p for p in here.parents if (p / "crates/alloy/src").is_dir())
g = (root / "crates/alloy-syntax/tests/fixtures/globalTypes.d.luau").read_text()
parents = {m.group(1): m.group(2) for m in re.finditer(r"^declare (?:extern type|class) (\w+)(?: extends (\w+))?", g, re.M)}
def is_instance(n):
    seen = set()
    while n and n not in seen:
        if n == "Instance":
            return True
        seen.add(n)
        n = parents.get(n)
    return False
instances = sorted(n for n in parents if is_instance(n))
# The file also carries names as `export type`, and the lists were
# extended by hand for a few of them. A regeneration keeps them.
classes_rs = root / "crates/alloy/src/roblox_classes.rs"
kept = set(re.findall(r'^    "(\w+)",$', classes_rs.read_text(), re.M)) if classes_rs.exists() else set()
datatypes = sorted({n for n in parents if not is_instance(n)} | (kept - set(instances)))
lines = ["//! Roblox class names, generated from luau-lsp's `globalTypes.d.luau` by",
         "//! `scripts/gen-roblox-classes.py`. `INSTANCE_CLASSES` descend from",
         "//! `Instance`, so `x is Name` emits an `IsA` check; `DATATYPES` are the",
         "//! other declared classes, so it emits a `typeof` check.", "",
         "pub const INSTANCE_CLASSES: &[&str] = &["]
lines += [f'    "{n}",' for n in instances] + ["];", "", "pub const DATATYPES: &[&str] = &["]
lines += [f'    "{n}",' for n in datatypes] + ["];", ""]
(root / "crates/alloy/src/roblox_classes.rs").write_text("\n".join(lines))
print("instances", len(instances), "datatypes", len(datatypes))

# The type each class writes for a property, for the markup check. Only
# a property whose type is one name is kept: a method or an overload set
# is not a value a tag's attribute carries.
own = {name: [] for name in parents}
current = None
for line in g.splitlines():
    head = re.match(r"declare (?:extern type|class) (\w+)", line)
    if head:
        current = head.group(1)
        continue
    member = re.match(r"\t(\w+): (\w+)$", line) if current else None
    if member:
        own[current].append(member.groups())

props = ["//! The type a Roblox class writes for a property, generated from",
         "//! luau-lsp's `globalTypes.d.luau` by `scripts/gen-roblox-classes.py`.",
         "//! Each entry holds the class it extends and the properties the class",
         "//! declares itself, so a lookup walks the chain.", "",
         "/// One class: what it extends, and the properties it declares.",
         "pub struct Class {",
         "    pub name: &'static str,",
         "    pub parent: &'static str,",
         "    pub properties: &'static [(&'static str, &'static str)],",
         "}", "",
         "/// Every declared class, in name order.",
         "#[rustfmt::skip]",
         "pub const CLASSES: &[Class] = &["]
for name in sorted(own):
    members = "".join(f'("{k}", "{v}"), ' for k, v in sorted(own[name]))
    props.append(f'    Class {{ name: "{name}", parent: "{parents[name] or ""}", properties: &[{members}] }},')
props += ["];", "",
          "/// The type a class writes for a property, walking what it extends.",
          "/// `None` when no class in the chain declares the name with a type",
          "/// this table keeps.",
          "pub fn property_type(class: &str, property: &str) -> Option<&'static str> {",
          "    let mut name = class;",
          "",
          "    for _ in 0..CLASSES.len() {",
          "        let at = CLASSES.binary_search_by_key(&name, |c| c.name).ok()?;",
          "        let found = CLASSES[at].properties.iter().find(|(p, _)| *p == property);",
          "",
          "        if let Some((_, ty)) = found {",
          "            return Some(ty);",
          "        }",
          "",
          "        if CLASSES[at].parent.is_empty() {",
          "            return None;",
          "        }",
          "",
          "        name = CLASSES[at].parent;",
          "    }",
          "",
          "    None",
          "}", ""]
(root / "crates/alloy/src/roblox_props.rs").write_text("\n".join(props))
print("classes with types", sum(1 for v in own.values() if v))
