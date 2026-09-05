#!/usr/bin/env python3
"""Regenerates crates/alloy/src/roblox_classes.rs from the vendored
globalTypes.d.luau. Run it after refreshing the definitions file."""
import pathlib, re
root = pathlib.Path(__file__).resolve().parent.parent
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
datatypes = sorted(n for n in parents if not is_instance(n))
lines = ["//! Roblox class names, generated from luau-lsp's `globalTypes.d.luau` by",
         "//! `scripts/gen-roblox-classes.py`. `INSTANCE_CLASSES` descend from",
         "//! `Instance`, so `x is Name` emits an `IsA` check; `DATATYPES` are the",
         "//! other declared classes, so it emits a `typeof` check.", "",
         "pub const INSTANCE_CLASSES: &[&str] = &["]
lines += [f'    "{n}",' for n in instances] + ["];", "", "pub const DATATYPES: &[&str] = &["]
lines += [f'    "{n}",' for n in datatypes] + ["];", ""]
(root / "crates/alloy/src/roblox_classes.rs").write_text("\n".join(lines))
print("instances", len(instances), "datatypes", len(datatypes))
