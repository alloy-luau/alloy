//! The two Luau configuration files: `.luaurc`, which is JSON, and
//! `.config.luau`, a Luau chunk that returns `{ luau = { ... } }`. The
//! build, the server, and `alloy init` read and write both.

use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Expr, Stmt, TableField, TokSpan};
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
                TableField::Named { name, value } => (text_of(text, toks, *name), value),

                TableField::Computed {
                    key: Expr::String(k),
                    value,
                } => (unquote(text_of(text, toks, *k)), value),

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

/// The source text of a token span.
fn text_of<'s>(src: &'s str, toks: &[Tok], span: TokSpan) -> &'s str {
    let a = toks[span.start as usize].start as usize;
    let b = toks[span.end as usize - 1].end as usize;

    &src[a..b]
}

/// The value of a named field of a table literal.
fn field<'a>(src: &str, toks: &[Tok], table: &'a Expr, name: &str) -> Option<&'a Expr> {
    let Expr::Table { fields, .. } = table else {
        return None;
    };

    fields.iter().find_map(|f| match f {
        TableField::Named { name: n, value } if text_of(src, toks, *n) == name => Some(value),

        TableField::Computed {
            key: Expr::String(k),
            value,
        } if unquote(text_of(src, toks, *k)) == name => Some(value),

        _ => None,
    })
}

fn string_of(src: &str, toks: &[Tok], e: &Expr) -> Option<String> {
    match e {
        Expr::String(s) => Some(unquote(text_of(src, toks, *s)).to_string()),

        _ => None,
    }
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
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
    fn a_rendered_config_luau_reads_back() {
        let c = LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: vec![("lest".to_string(), ".lest/core".to_string())],
        };
        assert_eq!(parse_config_luau(&render_config_luau(&c)).unwrap(), c);
        assert_eq!(parse_luaurc(&render_luaurc(&c)).unwrap(), c);
    }
}
