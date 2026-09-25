//! The std by name: the module each name sits in, the names the
//! language owns, and the names a project keeps ambient. See the
//! config-std-imports RFC.
//!
//! The emit writes every std name as `__alloy.Name` whatever this
//! module says, so a name the file does not reach still runs. The rule
//! here decides what the source may write bare.

use std::collections::HashSet;

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use crate::lint::Fix;

/// The spec every std module sits under. The spec alone is the facade,
/// which re-exports every module.
pub const PREFIX: &str = "@alloy/std";

/// Each std module and the names it holds. A name sits in one module.
pub const MODULES: &[(&str, &[&str])] = &[
    (
        "collections",
        &[
            "HashMap", "Set", "BitSet", "Queue", "Heap", "Array", "Symbol",
        ],
    ),
    ("iter", &["Iter"]),
    ("result", &["Result", "Ok", "Err"]),
    ("async", &["Future", "Scope"]),
    ("signal", &["Signal", "SignalConnection", "Signalish"]),
    (
        "traits",
        &[
            "Display",
            "Debug",
            "Clone",
            "Default",
            "Eq",
            "PartialEq",
            "Ord",
            "Add",
            "Sub",
            "Mul",
            "Div",
        ],
    ),
    ("serde", &["Serialize", "Deserialize"]),
    ("types", &["Partial", "Readonly", "Sink"]),
    ("roblox", &["R15Character", "R6Character", "Attributes"]),
];

/// The module a std name sits in.
pub fn module_of(name: &str) -> Option<&'static str> {
    MODULES
        .iter()
        .find(|(_, names)| names.contains(&name))
        .map(|(module, _)| *module)
}

/// The spec that imports a std name: `@alloy/std/collections`.
pub fn spec_of(name: &str) -> Option<String> {
    module_of(name).map(|m| format!("{PREFIX}/{m}"))
}

/// Whether a name is one the std exports.
pub fn is_std_name(name: &str) -> bool {
    module_of(name).is_some()
}

/// The module a std spec names: `Some("")` for the facade, `Some("iter")`
/// for `@alloy/std/iter`, and `None` for a spec outside the std. A
/// module the std does not have still answers, so the caller reports it.
pub fn module_of_spec(spec: &str) -> Option<&str> {
    let rest = spec.strip_prefix(PREFIX)?;

    match rest.strip_prefix('/') {
        Some(module) => Some(module),

        None => rest.is_empty().then_some(""),
    }
}

/// The names a std module holds, `None` when the std has no such
/// module. The facade holds every name.
pub fn names_in(module: &str) -> Option<Vec<&'static str>> {
    if module.is_empty() {
        return Some(
            MODULES
                .iter()
                .flat_map(|(_, n)| n.iter().copied())
                .collect(),
        );
    }

    MODULES
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, n)| n.to_vec())
}

/// Whether the language owns a name: a keyword, a literal, or an
/// operator writes it, so a file reaches it with no import under every
/// `[std] globals`. `async` returns a `Future`, `try` a `Result` of `Ok`
/// and `Err`, `[ ]` builds an `Array`, and `==`, `<`, `+`, and `tostring`
/// call the traits.
pub fn owned(name: &str) -> bool {
    matches!(name, "Future" | "Result" | "Ok" | "Err" | "Array")
        || module_of(name) == Some("traits")
}

/// `[std] globals`: which std names a file writes with no import.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Globals {
    /// Every std name is ambient.
    All,
    /// Only the names the language owns are ambient.
    #[default]
    None,
    /// These names are ambient too.
    List(Vec<String>),
}

impl Globals {
    /// Whether a file reaches a std name with no import.
    pub fn ambient(&self, name: &str) -> bool {
        owned(name)
            || match self {
                Globals::All => true,

                Globals::None => false,

                Globals::List(names) => names.iter().any(|n| n == name),
            }
    }
}

impl Serialize for Globals {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Globals::All => s.serialize_str("all"),

            Globals::None => s.serialize_str("none"),

            Globals::List(names) => names.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Globals {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = Globals;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("\"all\", \"none\", or a list of std names")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Globals, E> {
                match v {
                    "all" => Ok(Globals::All),

                    "none" => Ok(Globals::None),

                    other => Err(E::custom(format!(
                        "`{other}` is no value of globals; write \"all\", \"none\", or a list of std names"
                    ))),
                }
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Globals, A::Error> {
                let mut names = Vec::new();

                while let Some(name) = seq.next_element::<String>()? {
                    if !is_std_name(&name) {
                        return Err(de::Error::custom(format!(
                            "`{name}` is no std name; the std exports {}",
                            every_name().join(", ")
                        )));
                    }

                    names.push(name);
                }

                Ok(Globals::List(names))
            }
        }

        d.deserialize_any(V)
    }
}

/// Every std name, in module order.
pub fn every_name() -> Vec<&'static str> {
    names_in("").unwrap_or_default()
}

/// The report for a std name a file writes with no import.
pub fn missing_message(name: &str) -> String {
    let spec = spec_of(name).unwrap_or_else(|| PREFIX.to_string());

    format!("`{name}` is in the std; write `import {{ {name} }} from \"{spec}\"`")
}

/// The name a `missing_message` report names.
pub fn missing_name(message: &str) -> Option<&str> {
    let rest = message.strip_prefix('`')?;
    let (name, tail) = rest.split_once('`')?;

    (tail.starts_with(" is in the std; write `import {") && is_std_name(name)).then_some(name)
}

/// The std names the imports of a source bind under their own names:
/// `import { HashMap } from "@alloy/std/collections"` binds `HashMap`.
/// A name under an alias binds the alias, which is no std name.
pub fn imported(source: &str) -> HashSet<String> {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let mut out = HashSet::new();
    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return out;
    };
    let toks = &parsed.lexed.toks;

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Import(i) = stmt else {
            continue;
        };
        let spec = i.path.text(source, toks).trim_matches(['"', '\'']);

        if module_of_spec(spec).is_none() {
            continue;
        }

        let specs = match &i.kind {
            ImportKind::Named(s) | ImportKind::TypeOnly(s) | ImportKind::Both(_, s) => s,

            _ => continue,
        };

        for s in specs.iter().filter(|s| s.alias.is_none()) {
            out.insert(s.name.text(source, toks).to_string());
        }
    }

    out
}

/// The rewrites that import each of `names` from its std module. A name
/// joins the file's `import { } from` list of the facade or of its
/// module; the rest go on new lines below the file's last import, one
/// line per module.
pub fn import_fixes(src: &str, names: &[&str]) -> Vec<Fix> {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let imports: Vec<&alloy_syntax::ast::Import> = parsed
        .chunk
        .block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Import(i) => Some(i),

            _ => None,
        })
        .collect();
    let have = imported(src);
    // The list a name joins: the end of its last entry, keyed by spec.
    let list_end = |spec: &str| {
        imports.iter().find_map(|i| {
            let written = i.path.text(src, toks).trim_matches(['"', '\'']);

            match &i.kind {
                ImportKind::Named(specs) if written == spec => specs.last().map(|last| {
                    let end = last.alias.unwrap_or(last.name);

                    toks[end.end as usize - 1].end
                }),

                _ => None,
            }
        })
    };
    // The quote an import already uses, else the one most strings in the
    // file use, which the formatter keeps to the project's style. A tie
    // takes the formatter's default, single.
    let quote = imports
        .first()
        .and_then(|i| i.path.text(src, toks).chars().next())
        .filter(|c| *c == '\'' || *c == '"')
        .unwrap_or_else(|| {
            let (single, double) = toks
                .iter()
                .filter(|t| matches!(t.kind, alloy_syntax::lexer::TokKind::Str { .. }))
                .fold((0, 0), |(s, d), t| match src.as_bytes()[t.start as usize] {
                    b'\'' => (s + 1, d),

                    b'"' => (s, d + 1),

                    _ => (s, d),
                });

            if double > single { '"' } else { '\'' }
        });
    let mut joins: Vec<(u32, Vec<&str>)> = Vec::new();
    let mut lines: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for name in names {
        let Some(module) = module_of(name) else {
            continue;
        };

        if have.contains(*name) || !seen.insert(name) {
            continue;
        }

        let spec = format!("{PREFIX}/{module}");

        match list_end(PREFIX).or_else(|| list_end(&spec)) {
            Some(at) => match joins.iter_mut().find(|(a, _)| *a == at) {
                Some((_, list)) => list.push(name),

                None => joins.push((at, vec![name])),
            },

            None => match lines.iter_mut().find(|(m, _)| *m == module) {
                Some((_, list)) => list.push(name),

                None => lines.push((module, vec![name])),
            },
        }
    }

    let mut fixes: Vec<Fix> = joins
        .into_iter()
        .map(|(at, list)| {
            let text: String = list.iter().map(|n| format!(", {n}")).collect();

            Fix::new(src, at, at, text)
        })
        .collect();

    if !lines.is_empty() {
        let q = quote;
        let text: String = lines
            .iter()
            .map(|(module, list)| {
                format!(
                    "import {{ {} }} from {q}{PREFIX}/{module}{q}\n",
                    list.join(", ")
                )
            })
            .collect();
        let at = insertion_offset(
            src,
            imports.last().map(|i| toks[i.span.end as usize - 1].end),
        );
        fixes.push(Fix::new(src, at, at, text));
    }

    fixes.sort_by_key(|f| f.start);
    fixes
}

/// Where a new import line goes: the start of the line after the last
/// import, else after the `--!` directives that open the file.
fn insertion_offset(src: &str, last_import_end: Option<u32>) -> u32 {
    if let Some(end) = last_import_end {
        return match src[end as usize..].find('\n') {
            Some(n) => end + n as u32 + 1,

            None => src.len() as u32,
        };
    }

    let mut at = 0;

    for line in src.split_inclusive('\n') {
        if !line.trim_start().starts_with("--!") {
            break;
        }

        at += line.len();
    }

    at as u32
}

/// A source with every fix applied, last first so the offsets hold.
pub fn apply(src: &str, fixes: &[Fix]) -> String {
    let mut text = src.to_string();
    let mut sorted: Vec<&Fix> = fixes.iter().collect();
    sorted.sort_by_key(|f| std::cmp::Reverse(f.start));

    for f in sorted {
        text.replace_range(f.start as usize..f.end as usize, &f.replacement);
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_sits_in_one_module() {
        let mut seen = HashSet::new();

        for (_, names) in MODULES {
            for n in *names {
                assert!(seen.insert(*n), "{n} sits in two modules");
            }
        }

        // Every name the compiler reads as ambient has a module.
        for n in crate::desugar::AMBIENT
            .iter()
            .chain(crate::desugar::AMBIENT_TYPES)
        {
            assert!(is_std_name(n), "{n} has no module");
        }
    }

    /// `alloy doc std` lists each module with every name it holds.
    #[test]
    fn the_article_lists_every_module() {
        let article = crate::docs::lookup("topic:std").expect("the std article");

        for (module, names) in MODULES {
            let line = format!("{PREFIX}/{module}");
            let row = article
                .lines()
                .find(|l| l.starts_with(&format!("{line} ")))
                .unwrap_or_else(|| panic!("no row for {line}"));

            for n in *names {
                assert!(
                    row.split_whitespace().any(|w| w == *n),
                    "{n} missing from {row}"
                );
            }
        }
    }

    #[test]
    fn the_setting_decides_what_is_ambient() {
        assert!(Globals::All.ambient("HashMap"));
        assert!(!Globals::None.ambient("HashMap"));
        assert!(Globals::None.ambient("Ok") && Globals::None.ambient("Eq"));
        assert!(Globals::List(vec!["Signal".into()]).ambient("Signal"));
        assert!(!Globals::List(vec!["Signal".into()]).ambient("Iter"));

        let read = |text: &str| {
            toml::from_str::<toml::Table>(&format!("g = {text}"))
                .map(|t| t["g"].clone().try_into::<Globals>())
        };
        assert_eq!(read("\"all\"").unwrap().unwrap(), Globals::All);
        assert_eq!(
            read("[\"Iter\"]").unwrap().unwrap(),
            Globals::List(vec!["Iter".into()])
        );
        assert!(read("[\"Nope\"]").unwrap().is_err());
    }

    #[test]
    fn the_report_names_its_import_and_reads_back() {
        let m = missing_message("HashMap");
        assert_eq!(
            m,
            "`HashMap` is in the std; write `import { HashMap } from \"@alloy/std/collections\"`"
        );
        assert_eq!(missing_name(&m), Some("HashMap"));
        assert_eq!(module_of_spec("@alloy/std"), Some(""));
        assert_eq!(module_of_spec("@alloy/std/iter"), Some("iter"));
        assert_eq!(module_of_spec("@alloy/stdx"), None);
    }

    #[test]
    fn the_fix_joins_a_list_or_writes_a_line() {
        let src = "--!strict\nimport { a } from './a'\nimport { Set } from '@alloy/std/collections'\n\nlocal m = HashMap.new()\n";
        let out = apply(
            src,
            &import_fixes(src, &["HashMap", "Iter", "Signal", "HashMap"]),
        );
        assert_eq!(
            out,
            "--!strict\nimport { a } from './a'\nimport { Set, HashMap } from '@alloy/std/collections'\nimport { Iter } from '@alloy/std/iter'\nimport { Signal } from '@alloy/std/signal'\n\nlocal m = HashMap.new()\n"
        );

        // A file with no import takes the line below its directives, in
        // the quote its strings use, else the formatter's single quote.
        // A name it imports already takes nothing.
        let src = "--!strict\nlocal q = Queue.new()\n";
        assert_eq!(
            apply(src, &import_fixes(src, &["Queue"])),
            "--!strict\nimport { Queue } from '@alloy/std/collections'\nlocal q = Queue.new()\n"
        );
        let src = "local q = Queue.new(\"a\")\n";
        assert_eq!(
            apply(src, &import_fixes(src, &["Queue"])),
            "import { Queue } from \"@alloy/std/collections\"\nlocal q = Queue.new(\"a\")\n"
        );
        let src = "import { Queue } from \"@alloy/std\"\nlocal q = Queue.new()\n";
        assert!(import_fixes(src, &["Queue"]).is_empty());

        // The facade takes every name.
        let src = "import { Queue } from \"@alloy/std\"\nlocal q = Iter\n";
        assert_eq!(
            apply(src, &import_fixes(src, &["Iter"])),
            "import { Queue, Iter } from \"@alloy/std\"\nlocal q = Iter\n"
        );
    }
}
