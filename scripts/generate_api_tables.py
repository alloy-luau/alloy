#!/usr/bin/env python3
"""Regenerates crates/luaux/src/roblox/generated.rs from the Roblox API dump.

Usage:
    curl -sL -o api-dump.json \
      https://raw.githubusercontent.com/MaximumADHD/Roblox-Client-Tracker/roblox/API-Dump.json
    python3 scripts/generate_api_tables.py api-dump.json
    cargo fmt --all

Only what the compiler needs is kept: creatable class names, each class's own
settable properties and events, and the superclass link. Inheritance is resolved
at lookup time rather than flattened here, which keeps the table ~10x smaller.

A property tagged Hidden is kept. The tag hides it in the Studio property
panel, and a script still sets it: TextLabel.Font and BasePart.Position both
carry it. NotScriptable is what a script cannot set.
"""
import json
import sys
from pathlib import Path

HEADER = '''//! Generated from the Roblox API dump. Do not edit.
//!
//! Regenerate with `python3 scripts/generate_api_tables.py api-dump.json`.
//!
//! Members are the ones each class *declares*; inheritance is resolved by
//! walking `superclass` at lookup time (see the parent module). Properties are
//! filtered to those a script can assign: no ReadOnly, no NotScriptable, no
//! security. `deprecated` names the class's own members Roblox deprecates.

pub struct ClassInfo {
    pub name: &'static str,
    pub superclass: &'static str,
    pub creatable: bool,
    pub properties: &'static [&'static str],
    pub events: &'static [&'static str],
    pub deprecated: &'static [&'static str],
}

'''

DEPRECATED_HEADER = '''
/// Member names Roblox marks deprecated, sorted.
///
/// Roblox keeps old spellings alongside modern ones: `brickColor` beside
/// `BrickColor`, `childAdded` beside `ChildAdded`. Those pairs differ only by
/// case, so any casing normalisation merges them, and this list is what breaks
/// the tie (see docs/adr/0001-casing-key.md).
pub static DEPRECATED: &[&str] = &[
'''


def settable(member):
    if member["MemberType"] != "Property":
        return False
    security = member.get("Security") or {}
    if security.get("Read", "None") != "None" or security.get("Write", "None") != "None":
        return False
    tags = member.get("Tags") or []
    return "ReadOnly" not in tags and "NotScriptable" not in tags


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2

    dump = json.loads(Path(sys.argv[1]).read_text())
    out = [HEADER, "pub static CLASSES: &[ClassInfo] = &[\n"]
    classes = properties = events = 0
    deprecated = set()

    for klass in sorted(dump["Classes"], key=lambda k: k["Name"]):
        tags = klass.get("Tags") or []
        members = klass.get("Members") or []

        own_properties = sorted({m["Name"] for m in members if settable(m)})
        own_events = sorted({m["Name"] for m in members if m["MemberType"] == "Event"})
        own_deprecated = sorted(
            {
                m["Name"]
                for m in members
                if m["MemberType"] in ("Property", "Event")
                and "Deprecated" in (m.get("Tags") or [])
            }
        )
        deprecated.update(own_deprecated)

        classes += 1
        properties += len(own_properties)
        events += len(own_events)

        def render(names):
            return "&[" + ", ".join(f'"{n}"' for n in names) + "]" if names else "&[]"

        out.append(
            "    ClassInfo {{ name: \"{}\", superclass: \"{}\", creatable: {}, "
            "properties: {}, events: {}, deprecated: {} }},\n".format(
                klass["Name"],
                klass.get("Superclass", "<<Root>>"),
                "false" if "NotCreatable" in tags else "true",
                render(own_properties),
                render(own_events),
                render(own_deprecated),
            )
        )

    out.append("];\n")

    out.append(DEPRECATED_HEADER)
    for name in sorted(deprecated):
        out.append(f'    "{name}",\n')
    out.append("];\n")

    target = Path("crates/luaux/src/roblox/generated.rs")
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(out))

    print(
        f"{target}: {classes} classes, {properties} properties, {events} events, "
        f"{len(deprecated)} deprecated"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
