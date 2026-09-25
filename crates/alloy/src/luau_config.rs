//! The two Luau configuration files: `.luaurc`, which is JSON, and
//! `.config.luau`, a Luau chunk that returns `{ luau = { ... } }`. The
//! build, the server, and `alloy init` read and write both.

use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Expr, Stmt, TableField};
use alloy_syntax::lexer::Tok;

/// The keys the tools read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LuauConfig {
    /// `strict`, `nonstrict`, or `nocheck`.
    pub language_mode: Option<String>,
    /// Alias name to the path it stands for, unresolved.
    pub aliases: Vec<(String, String)>,
}

/// The file names, in the order Luau looks for them.
pub const FILE_NAMES: [&str; 2] = [".luaurc", ".config.luau"];

/// Reads the configuration of `dir` from whichever file it has. With
/// both, `.config.luau` wins, as in Luau.
pub fn read_dir(dir: &Path) -> Option<(PathBuf, LuauConfig)> {
    let luau = dir.join(".config.luau");

    if let Ok(text) = std::fs::read_to_string(&luau)
        && let Some(c) = parse_config_luau(&text)
    {
        return Some((luau, c));
    }

    let rc = dir.join(".luaurc");
    let text = std::fs::read_to_string(&rc).ok()?;

    parse_luaurc(&text).map(|c| (rc, c))
}

/// Whether `dir` has a Luau configuration file of either name.
pub fn has_config(dir: &Path) -> bool {
    FILE_NAMES.iter().any(|n| dir.join(n).is_file())
}

/// Parses `.luaurc`.
pub fn parse_luaurc(text: &str) -> Option<LuauConfig> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let language_mode = json
        .get("languageMode")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut aliases: Vec<(String, String)> = json
        .get("aliases")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|p| (k.clone(), p.to_string())))
                .collect()
        })
        .unwrap_or_default();
    aliases.sort();

    Some(LuauConfig {
        language_mode,
        aliases,
    })
}

/// Parses `.config.luau`: the chunk must return a table literal whose
/// `luau` field is a table literal. A computed value is not read.
pub fn parse_config_luau(text: &str) -> Option<LuauConfig> {
    let parsed = alloy_syntax::parse_lenient(text, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let returned = parsed.chunk.block.stmts.iter().find_map(|s| match s {
        Stmt::Return(r) => r.values.first(),

        _ => None,
    })?;
    let luau = field(text, toks, returned, "luau")?;
    let language_mode =
        field(text, toks, luau, "languagemode").and_then(|e| string_of(text, toks, e));
    let mut aliases = Vec::new();

    if let Some(Expr::Table { fields, .. }) = field(text, toks, luau, "aliases") {
        for f in fields {
            let (key, value) = match f {
                TableField::Named { name, value } => (name.text(text, toks), value),

                TableField::Computed {
                    key: Expr::String(k),
                    value,
                } => (unquote(k.text(text, toks)), value),

                _ => continue,
            };

            if let Some(v) = string_of(text, toks, value) {
                aliases.push((key.to_string(), v));
            }
        }
    }

    aliases.sort();

    Some(LuauConfig {
        language_mode,
        aliases,
    })
}

/// The Luau key a `.config.luau` writes at the top level, outside the
/// `luau` table. Luau reads the keys under `luau` alone, so it ignores
/// one written higher, beside a `luau` table or with none. `None` when
/// the file writes no key this reads at the top level.
pub fn misplaced_key(text: &str) -> Option<&'static str> {
    let parsed = alloy_syntax::parse_lenient(text, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let returned = parsed.chunk.block.stmts.iter().find_map(|s| match s {
        Stmt::Return(r) => r.values.first(),

        _ => None,
    })?;

    ["aliases", "languagemode"]
        .into_iter()
        .find(|k| field(text, toks, returned, k).is_some())
}

/// The source text of a token span.
/// The value of a named field of a table literal.
fn field<'a>(src: &str, toks: &[Tok], table: &'a Expr, name: &str) -> Option<&'a Expr> {
    let Expr::Table { fields, .. } = table else {
        return None;
    };

    fields.iter().find_map(|f| match f {
        TableField::Named { name: n, value } if n.text(src, toks) == name => Some(value),

        TableField::Computed {
            key: Expr::String(k),
            value,
        } if unquote(k.text(src, toks)) == name => Some(value),

        _ => None,
    })
}

fn string_of(src: &str, toks: &[Tok], e: &Expr) -> Option<String> {
    match e {
        Expr::String(s) => Some(unquote(s.text(src, toks)).to_string()),

        _ => None,
    }
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
}

/// The alias `alloy init` writes for the runtime the build puts at the
/// output root.
pub const ALLOY_ALIAS: (&str, &str) = ("alloy", "./build/alloy");

/// Adds strict mode and the `@alloy` alias to an existing `.luaurc`,
/// each only when the file lacks it. Every other byte stays, comments
/// included, so the edit never rewrites what the user wrote. Returns
/// the text and the names of what it added.
pub fn add_defaults_luaurc(text: &str) -> Result<(String, Vec<String>), String> {
    let current = parse_luaurc(text).unwrap_or_default();
    let mut out = text.to_string();
    let mut added = Vec::new();

    if current.language_mode.is_none() {
        let edit = crate::jsonc::Edit::Set {
            path: crate::jsonc::path(&["languageMode"]),
            value: "\"strict\"".to_string(),
        };
        out = crate::jsonc::apply(&out, &[edit], "  ")?;
        added.push("strict mode".to_string());
    }

    if let Some(text) = add_alias_luaurc(&out, ALLOY_ALIAS)? {
        out = text;
        added.push(format!("@{}", ALLOY_ALIAS.0));
    }

    Ok((out, added))
}

/// Adds one alias to a `.luaurc`, or `None` when the file has it
/// already. Every other byte stays, comments included.
pub fn add_alias_luaurc(text: &str, alias: (&str, &str)) -> Result<Option<String>, String> {
    let current = parse_luaurc(text).unwrap_or_default();

    if current.aliases.iter().any(|(a, _)| a == alias.0) {
        return Ok(None);
    }

    let edit = crate::jsonc::Edit::Set {
        path: crate::jsonc::path(&["aliases", alias.0]),
        value: format!("\"{}\"", alias.1),
    };

    crate::jsonc::apply(text, &[edit], "  ").map(Some)
}

/// The same as `add_alias_luaurc`, for a `.config.luau`. The entry goes
/// in as text, so the rest of the chunk keeps its bytes. The error says
/// why a chunk cannot take the alias.
pub fn add_alias_config_luau(text: &str, alias: (&str, &str)) -> Result<Option<String>, String> {
    let current =
        parse_config_luau(text).ok_or("the chunk returns no table with a `luau` table")?;

    if current.aliases.iter().any(|(a, _)| a == alias.0) {
        return Ok(None);
    }

    let mut out = text.to_string();
    let (at, entry) = match table_body(&out, &["luau", "aliases"]) {
        Some(at) => (at, format!("\n            {} = \"{}\",", alias.0, alias.1)),

        None => (
            table_body(&out, &["luau"]).ok_or("the chunk has no `luau` table")?,
            format!(
                "\n        aliases = {{\n            {} = \"{}\",\n        }},",
                alias.0, alias.1
            ),
        ),
    };
    out.insert_str(at, &entry);

    Ok(Some(out))
}

/// The byte after the `{` that opens the table at `path` in the table
/// the chunk returns. A same-named table elsewhere, such as a top-level
/// `aliases` beside `luau`, is not the one Luau reads.
fn table_body(text: &str, path: &[&str]) -> Option<usize> {
    let parsed = alloy_syntax::parse_lenient(text, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let mut table = parsed.chunk.block.stmts.iter().find_map(|s| match s {
        Stmt::Return(r) => r.values.first(),

        _ => None,
    })?;

    for name in path {
        table = field(text, toks, table, name)?;
    }

    let Expr::Table { span, .. } = table else {
        return None;
    };

    Some(toks.get(span.start as usize)?.end as usize)
}

/// The same as `add_defaults_luaurc`, for a `.config.luau`. The entries
/// go in as text, so the rest of the chunk keeps its bytes. `None` when
/// the chunk is not a table literal this reader understands.
pub fn add_defaults_config_luau(text: &str) -> Option<(String, Vec<String>)> {
    let current = parse_config_luau(text)?;
    let mut out = text.to_string();
    let mut added = Vec::new();

    if let Some(text) = add_alias_config_luau(&out, ALLOY_ALIAS).ok()? {
        out = text;
        added.push(format!("@{}", ALLOY_ALIAS.0));
    }

    if current.language_mode.is_none() {
        let at = table_body(&out, &["luau"])?;
        out.insert_str(at, "\n        languagemode = \"strict\",");
        added.push("strict mode".to_string());
    }

    Some((out, added))
}

/// The `.config.luau` text for a configuration.
pub fn render_config_luau(c: &LuauConfig) -> String {
    let mut out = String::from("return {\n    luau = {\n");

    if let Some(m) = &c.language_mode {
        out.push_str(&format!("        languagemode = \"{m}\",\n"));
    }

    if !c.aliases.is_empty() {
        out.push_str("        aliases = {\n");

        for (k, v) in &c.aliases {
            let plain = k.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
                && !k.starts_with(|ch: char| ch.is_ascii_digit());

            if plain {
                out.push_str(&format!("            {k} = \"{v}\",\n"));
            } else {
                out.push_str(&format!("            [\"{k}\"] = \"{v}\",\n"));
            }
        }

        out.push_str("        },\n");
    }

    out.push_str("    },\n}\n");
    out
}

/// The `.luaurc` text for a configuration.
pub fn render_luaurc(c: &LuauConfig) -> String {
    let mut json = serde_json::Map::new();

    if let Some(m) = &c.language_mode {
        json.insert("languageMode".into(), serde_json::Value::String(m.clone()));
    }

    if !c.aliases.is_empty() {
        let mut aliases = serde_json::Map::new();

        for (k, v) in &c.aliases {
            aliases.insert(k.clone(), serde_json::Value::String(v.clone()));
        }

        json.insert("aliases".into(), serde_json::Value::Object(aliases));
    }

    let mut text = serde_json::to_string_pretty(&serde_json::Value::Object(json))
        .unwrap_or_else(|_| "{}".to_string());
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_templates_read_the_same() {
        let rc = parse_luaurc(crate::config::LUAURC_TEMPLATE).unwrap();
        let luau = parse_config_luau(crate::config::CONFIG_LUAU_TEMPLATE).unwrap();
        assert_eq!(rc, luau);
        assert_eq!(rc.language_mode.as_deref(), Some("strict"));
        assert_eq!(
            rc.aliases,
            vec![("alloy".to_string(), "./build/alloy".to_string())]
        );
    }

    #[test]
    fn an_existing_luaurc_gains_only_what_it_lacks() {
        let (text, added) =
            add_defaults_luaurc("{ \"aliases\": { \"pkg\": \"Packages\" } }\n").unwrap();
        assert_eq!(added, vec!["strict mode", "@alloy"]);
        let back = parse_luaurc(&text).unwrap();
        assert_eq!(back.language_mode.as_deref(), Some("strict"));
        assert_eq!(
            back.aliases,
            vec![
                ("alloy".to_string(), "./build/alloy".to_string()),
                ("pkg".to_string(), "Packages".to_string()),
            ]
        );

        // A second run changes nothing.
        let (again, added) = add_defaults_luaurc(&text).unwrap();
        assert!(added.is_empty());
        assert_eq!(again, text);

        // The user's own mode stays.
        let (text, added) = add_defaults_luaurc("{ \"languageMode\": \"nonstrict\" }\n").unwrap();
        assert_eq!(added, vec!["@alloy"]);
        assert_eq!(
            parse_luaurc(&text).unwrap().language_mode.as_deref(),
            Some("nonstrict")
        );
    }

    #[test]
    fn an_existing_config_luau_gains_only_what_it_lacks() {
        let (text, added) =
            add_defaults_config_luau("return {\n    luau = {\n        aliases = {\n            pkg = \"Packages\",\n        },\n    },\n}\n")
                .unwrap();
        assert_eq!(added, vec!["@alloy", "strict mode"]);
        let back = parse_config_luau(&text).unwrap();
        assert_eq!(back.language_mode.as_deref(), Some("strict"));
        assert_eq!(
            back.aliases,
            vec![
                ("alloy".to_string(), "./build/alloy".to_string()),
                ("pkg".to_string(), "Packages".to_string()),
            ]
        );

        let (again, added) = add_defaults_config_luau(&text).unwrap();
        assert!(again == text && added.is_empty(), "{again}");

        // A chunk with no `aliases` table gets one.
        let (text, added) = add_defaults_config_luau(
            "return {\n    luau = {\n        languagemode = \"nonstrict\",\n    },\n}\n",
        )
        .unwrap();
        assert_eq!(added, vec!["@alloy"]);
        let back = parse_config_luau(&text).unwrap();
        assert_eq!(back.language_mode.as_deref(), Some("nonstrict"));
        assert_eq!(
            back.aliases,
            vec![("alloy".to_string(), "./build/alloy".to_string())]
        );
    }

    #[test]
    fn a_rendered_config_luau_reads_back() {
        let c = LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: vec![("lest".to_string(), ".lest/core".to_string())],
        };
        assert_eq!(parse_config_luau(&render_config_luau(&c)).unwrap(), c);
        assert_eq!(parse_luaurc(&render_luaurc(&c)).unwrap(), c);
    }

    /// An `aliases` table beside `luau` is not the one Luau reads. The
    /// alias goes under `luau`, once, and the stray table reports.
    #[test]
    fn an_alias_goes_under_luau_past_a_stray_table() {
        let text = "return {\n\tluau = {\n\t\tlanguagemode = \"strict\",\n\t},\n\n\taliases = {\n\t\tlest = \".lest/core\",\n\t}\n}\n";
        let out = add_alias_config_luau(text, ("lest", ".lest/core"))
            .unwrap()
            .expect("the alias is new under `luau`");
        let read = parse_config_luau(&out).unwrap();

        assert_eq!(
            read.aliases,
            [("lest".to_string(), ".lest/core".to_string())]
        );
        assert_eq!(
            add_alias_config_luau(&out, ("lest", ".lest/core")),
            Ok(None)
        );
        assert_eq!(misplaced_key(text), Some("aliases"));
        assert_eq!(misplaced_key(&render_config_luau(&read)), None);
    }
}
