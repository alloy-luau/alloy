//! The completion contexts the proxy answers itself. The child sees the
//! emit, where an attribute, a macro call, a remote's side, or an import
//! no longer exists, so a completion there would list globals.

/// What the cursor sits in, from the text of its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// `@der|`: an attribute name. `sigil` is the byte offset of `@`, and
    /// `target` what the attribute would go on, when the position says.
    Attribute {
        prefix: String,
        sigil: usize,
        target: Option<&'static str>,
    },
    /// `@derive(Eq, De|`: a derive name.
    DeriveArg { prefix: String },
    /// `$dg|`: an intrinsic or a macro. `sigil` is the byte offset of `$`.
    Macro { prefix: String, sigil: usize },
    /// `remote X(...) from cl|`: a side. `after` narrows to `or` or to
    /// the other side.
    RemoteSide {
        prefix: String,
        after: Option<String>,
    },
    /// `remote X(...) |`: the `from`.
    RemoteFrom { prefix: String },
    /// `struct Name |`, `enum Name |`, `interface Name |`: the `as` that
    /// opens the body, and `extends` for an interface. The child would
    /// offer `assert` here.
    DeclarationAs { prefix: String, interface: bool },
    /// `import |` or `import type |`.
    ImportHead { prefix: String, type_only: bool },
    /// `import { a, b| } from "./m"`: names from the module.
    ImportNames {
        prefix: String,
        type_only: bool,
        spec: Option<String>,
        /// A name just ended, so `as` fits.
        after_name: bool,
    },
    /// `import * |`.
    ImportStar,
    /// `import * as M |` or `import { ... } |`: the `from`.
    ImportFrom,
    /// `attribute name(...) |`: the `on`.
    AttributeOn,
    /// `attribute name on fi|`: a target.
    AttributeTarget { prefix: String },
    /// Inside the string of `from "..."`, `require("...")`, or
    /// `import("...")`: a module path. `text` is what the string holds so
    /// far, and `start` the byte offset after the opening quote.
    ImportSpec { text: String, start: usize },
}

/// The string a module path is being typed in, when the cursor is inside
/// one after `from`, `require(`, or `import(`.
fn import_spec(src: &str, line_start: usize, offset: usize) -> Option<Context> {
    let before = &src[line_start..offset];
    let quote = before.rfind(['"', '\''])?;
    let text = &before[quote + 1..];

    if text.contains(['"', '\'']) {
        return None;
    }

    let head = before[..quote].trim_end();

    if !(head.ends_with("from") || head.ends_with("require(") || head.ends_with("import(")) {
        return None;
    }

    Some(Context::ImportSpec {
        text: text.to_string(),
        start: line_start + quote + 1,
    })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `struct Name `, `enum Name<T> `, `export interface Name `: the head
/// of a declaration whose body opener comes next. `Some(true)` for an
/// interface, which may take `extends` first.
fn declaration_head(head: &str) -> Option<bool> {
    let t = head.trim_start();
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let (rest, interface) = if let Some(r) = t.strip_prefix("struct ") {
        (r, false)
    } else if let Some(r) = t.strip_prefix("enum ") {
        (r, false)
    } else {
        (t.strip_prefix("interface ")?, true)
    };
    let rest = rest.trim_start();
    let name_len = rest.chars().take_while(|c| is_word(*c)).count();

    if name_len == 0 {
        return None;
    }

    let mut after = &rest[name_len..];

    if after.starts_with('<') {
        let close = after.find('>')?;
        after = &after[close + 1..];
    }

    (after.ends_with([' ', '\t']) && after.trim().is_empty()).then_some(interface)
}

/// The declaration keyword a line starts with, `export` aside.
fn declaration_word(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let t = t
        .strip_prefix("local ")
        .or_else(|| t.strip_prefix("const "))
        .map(str::trim_start)
        .unwrap_or(t);
    let t = t.strip_prefix("async ").map(str::trim_start).unwrap_or(t);

    for (word, target) in [
        ("function ", "function"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("remote ", "remote"),
        ("interface ", "interface"),
        ("type ", "type"),
    ] {
        if t.starts_with(word) {
            return Some(target);
        }
    }

    None
}

/// What an attribute at this position would go on: a remote's parameter
/// inside its parentheses, a field or a variant inside a struct or an
/// enum, or the declaration the next non-attribute line starts.
fn attribute_target(
    src: &str,
    line_start: usize,
    line_end: usize,
    head: &str,
) -> Option<&'static str> {
    let opens = head.matches('(').count();
    let closes = head.matches(')').count();

    if opens > closes {
        return if head.trim_start().starts_with("remote ")
            || head.trim_start().starts_with("export remote ")
        {
            Some("param")
        } else {
            None
        };
    }

    // An indented line sits in a body: the nearest column-zero line above
    // names it. A column-zero `end` or another statement ends the search.
    let indented = head.starts_with(' ') || head.starts_with('\t');

    if indented {
        for line in src[..line_start].lines().rev() {
            if line.trim().is_empty() || !line.starts_with(|c: char| !c.is_whitespace()) {
                continue;
            }

            return match declaration_word(line) {
                Some("struct") | Some("interface") => Some("field"),
                Some("enum") => Some("variant"),
                _ => None,
            };
        }

        return None;
    }

    // At column zero the attribute precedes a declaration: skip the other
    // attribute lines, blanks, and comments to the first one.
    for line in src[line_end..].lines().skip(1) {
        let t = line.trim_start();

        if t.is_empty() || t.starts_with('@') || t.starts_with("--") {
            continue;
        }

        return declaration_word(line);
    }

    None
}

/// The word the cursor is at the end of.
fn trailing_word(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    &text[start..]
}

pub fn detect(src: &str, offset: usize) -> Option<Context> {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);

    if let Some(spec) = import_spec(src, line_start, offset) {
        return Some(spec);
    }

    let line_end = src[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(src.len());
    let before = &src[line_start..offset];
    let line = &src[line_start..line_end];
    let prefix = trailing_word(before);
    let head = &before[..before.len() - prefix.len()];

    // A sigil right before the word.
    if head.ends_with('@') {
        return Some(Context::Attribute {
            prefix: prefix.to_string(),
            sigil: line_start + head.len() - 1,
            target: attribute_target(src, line_start, line_end, head),
        });
    }

    if head.ends_with('$') {
        return Some(Context::Macro {
            prefix: prefix.to_string(),
            sigil: line_start + head.len() - 1,
        });
    }

    if let Some(i) = head.rfind("@derive(")
        && !head[i..].contains(')')
    {
        return Some(Context::DeriveArg {
            prefix: prefix.to_string(),
        });
    }

    if let Some(interface) = declaration_head(head) {
        return Some(Context::DeclarationAs {
            prefix: prefix.to_string(),
            interface,
        });
    }

    let trimmed = before.trim_start();

    let attr_decl = trimmed
        .strip_prefix("export ")
        .unwrap_or(trimmed)
        .strip_prefix("attribute ");

    if let Some(rest) = attr_decl {
        let rest_head = &rest[..rest.len() - prefix.len()];

        if let Some(i) = rest_head.rfind(" on") {
            let after = &rest_head[i + " on".len()..];

            if after.is_empty() || after.starts_with(' ') {
                return Some(Context::AttributeTarget {
                    prefix: prefix.to_string(),
                });
            }
        }

        // The name, and the parameters when closed, then the cursor.
        let closed = rest_head.trim_end();
        let named = !closed.is_empty() && (!closed.contains('(') || closed.ends_with(')'));

        if named && rest_head.ends_with(' ') {
            return Some(Context::AttributeOn);
        }

        return None;
    }

    if trimmed.starts_with("remote ") || trimmed.starts_with("export remote ") {
        // The parameters closed and no `from` yet: the `from` comes next.
        let opens = head.matches('(').count();
        let closes = head.matches(')').count();

        if closes > 0 && opens == closes && !head.contains(" from") && head.ends_with(' ') {
            return Some(Context::RemoteFrom {
                prefix: prefix.to_string(),
            });
        }

        if let Some(i) = head.rfind(" from") {
            let tail = head[i + " from".len()..].trim();

            if tail.is_empty() {
                return Some(Context::RemoteSide {
                    prefix: prefix.to_string(),
                    after: None,
                });
            }

            let words: Vec<&str> = tail.split_whitespace().collect();

            match words.as_slice() {
                [side] if matches!(*side, "client" | "server") => {
                    return Some(Context::RemoteSide {
                        prefix: prefix.to_string(),
                        after: Some(format!("{side} ")),
                    });
                }

                [side, "or"] if matches!(*side, "client" | "server") => {
                    return Some(Context::RemoteSide {
                        prefix: prefix.to_string(),
                        after: Some(format!("{side} or")),
                    });
                }

                _ => {}
            }
        }

        return None;
    }

    if trimmed.starts_with("import")
        && (trimmed.len() == 6 || !is_word(trimmed.as_bytes()[6] as char))
    {
        let rest = head.trim_start()["import".len()..].trim_start();
        let type_only = rest.starts_with("type ") || rest == "type";
        let rest = rest
            .strip_prefix("type")
            .map(str::trim_start)
            .unwrap_or(rest);

        if rest.is_empty() {
            return Some(Context::ImportHead {
                prefix: prefix.to_string(),
                type_only,
            });
        }

        if let Some(open) = rest.find('{') {
            if rest[open..].contains('}') {
                return Some(Context::ImportFrom);
            }

            let inside = &rest[open + 1..];
            let after_name = inside.trim_end().chars().last().is_some_and(is_word)
                && inside.ends_with(' ')
                && prefix.is_empty();
            let spec = line
                .find("from")
                .and_then(|i| line[i..].split('"').nth(1))
                .map(str::to_string);

            return Some(Context::ImportNames {
                prefix: prefix.to_string(),
                type_only,
                spec,
                after_name,
            });
        }

        if let Some(after_star) = rest.strip_prefix('*') {
            let after_star = after_star.trim_start();

            if after_star.is_empty() {
                return Some(Context::ImportStar);
            }

            if let Some(named) = after_star.strip_prefix("as")
                && named.split_whitespace().count() == 1
                && named.ends_with(' ')
            {
                return Some(Context::ImportFrom);
            }

            return None;
        }

        // `import Name |`: a default import wants `from`.
        if rest.split_whitespace().count() == 1 && rest.ends_with(' ') {
            return Some(Context::ImportFrom);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(src: &str) -> Option<Context> {
        let offset = src.find('|').unwrap();
        detect(&src.replace('|', ""), offset)
    }

    #[test]
    fn a_declaration_name_wants_as() {
        assert_eq!(
            at("enum Test |"),
            Some(Context::DeclarationAs {
                prefix: String::new(),
                interface: false
            })
        );
        assert_eq!(
            at("export struct Vec2<T> as|"),
            Some(Context::DeclarationAs {
                prefix: "as".to_string(),
                interface: false
            })
        );
        assert_eq!(
            at("interface Entity ex|"),
            Some(Context::DeclarationAs {
                prefix: "ex".to_string(),
                interface: true
            })
        );
        assert_eq!(at("enum Test as |"), None);
        assert_eq!(at("enum |"), None);
    }

    #[test]
    fn sigils() {
        assert_eq!(
            at("@der|"),
            Some(Context::Attribute {
                prefix: "der".to_string(),
                sigil: 0,
                target: None
            })
        );
        assert_eq!(
            at("local x = $d|"),
            Some(Context::Macro {
                prefix: "d".to_string(),
                sigil: 10
            })
        );
        assert_eq!(
            at("@derive(Eq, De|"),
            Some(Context::DeriveArg {
                prefix: "De".to_string()
            })
        );
        assert_eq!(at("@derive(Eq) |"), None);
    }

    #[test]
    fn remote_sides() {
        assert_eq!(
            at("remote Ping(n: number) from |"),
            Some(Context::RemoteSide {
                prefix: String::new(),
                after: None
            })
        );
        assert_eq!(
            at("export remote Ping(n: number) from client or s|"),
            Some(Context::RemoteSide {
                prefix: "s".to_string(),
                after: Some("client or".to_string())
            })
        );
        assert_eq!(
            at("export remote Test() |"),
            Some(Context::RemoteFrom {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("remote function Get(id: number): Profile fr|"),
            Some(Context::RemoteFrom {
                prefix: "fr".to_string()
            })
        );
        assert_eq!(at("remote Test(|"), None);
        assert_eq!(at("local from = 1 |"), None);
    }

    #[test]
    fn import_specs() {
        assert_eq!(
            at("import { a } from \"./mod|\""),
            Some(Context::ImportSpec {
                text: "./mod".to_string(),
                start: 19
            })
        );
        assert_eq!(
            at("local m = require('@pack|"),
            Some(Context::ImportSpec {
                text: "@pack".to_string(),
                start: 19
            })
        );
        assert_eq!(at("local s = \"from |\""), None);
    }

    fn target_of(src: &str) -> Option<&'static str> {
        match at(src) {
            Some(Context::Attribute { target, .. }) => target,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn attribute_targets_follow_the_position() {
        assert_eq!(target_of("@|\nstruct V as\nend\n"), Some("struct"));
        assert_eq!(
            target_of("@|\n@derive(Eq)\n-- note\nexport enum E as A end\n"),
            Some("enum")
        );
        assert_eq!(
            target_of("@|\nlocal async function f() end\n"),
            Some("function")
        );
        assert_eq!(
            target_of("@|\nexport remote Ping(n: number) from client\n"),
            Some("remote")
        );
        assert_eq!(target_of("remote Ping(@|"), Some("param"));
        assert_eq!(
            target_of("struct V as\n    @|\n    x: number\nend\n"),
            Some("field")
        );
        assert_eq!(
            target_of("enum E as\n    @|\n    A\nend\n"),
            Some("variant")
        );
        assert_eq!(target_of("struct V as\nend\n    @|\n"), None);
        assert_eq!(target_of("@|\n"), None);
    }

    #[test]
    fn attribute_declarations() {
        assert_eq!(
            at("attribute icon(asset: string) |"),
            Some(Context::AttributeOn)
        );
        assert_eq!(at("export attribute skip |"), Some(Context::AttributeOn));
        assert_eq!(at("attribute skip o|"), Some(Context::AttributeOn));
        assert_eq!(
            at("attribute icon(asset: string) on |"),
            Some(Context::AttributeTarget {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("attribute icon(asset: string) on struct, en|"),
            Some(Context::AttributeTarget {
                prefix: "en".to_string()
            })
        );
        assert_eq!(at("attribute icon(asset: |"), None);
    }

    #[test]
    fn imports() {
        assert_eq!(
            at("import |"),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: false
            })
        );
        assert_eq!(
            at("import type |"),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: true
            })
        );
        assert_eq!(
            at("import { a, b| } from \"./m\""),
            Some(Context::ImportNames {
                prefix: "b".to_string(),
                type_only: false,
                spec: Some("./m".to_string()),
                after_name: false
            })
        );
        assert_eq!(
            at("import { a |"),
            Some(Context::ImportNames {
                prefix: String::new(),
                type_only: false,
                spec: None,
                after_name: true
            })
        );
        assert_eq!(at("import * |"), Some(Context::ImportStar));
        assert_eq!(at("import * as M |"), Some(Context::ImportFrom));
        assert_eq!(at("import { a } |"), Some(Context::ImportFrom));
        assert_eq!(at("import Panel |"), Some(Context::ImportFrom));
    }
}
