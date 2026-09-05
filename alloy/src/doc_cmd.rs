//! `alloy doc`: the documentation table on the terminal.
//!
//! With no topic the command lists what it can explain, in groups. With
//! one it prints that entry: a keyword, an operator, an intrinsic, an
//! attribute, a std name, a lint, or an article such as `strict`.

use std::process::ExitCode;

use alloy::docs::{self, TABLE};
use alloy::lint::{Group, LINTS, Level};

use crate::highlight::{self, Mode};
use crate::ui::{self, BOLD, RESET};

/// Whether a key belongs to a group.
type Pick = fn(&str) -> bool;

/// The groups of the index, by key shape.
const GROUPS: &[(&str, Pick)] = &[
    ("Articles", |k| k.starts_with("topic:")),
    ("Keywords", |k| {
        k.chars().all(|c| c.is_ascii_lowercase()) && !k.is_empty()
    }),
    ("Operators", |k| {
        !k.chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '$' || c == '@')
    }),
    ("Intrinsics", |k| k.starts_with('$')),
    ("Attributes", |k| k.starts_with('@')),
    ("Derives", |k| k.starts_with("derive:")),
    ("Std", |k| {
        k.chars().next().is_some_and(|c| c.is_ascii_uppercase())
    }),
];

pub fn run(args: &[String]) -> ExitCode {
    let color = ui::want_color();
    let topic = args.iter().find(|a| !a.starts_with("--"));

    if args.iter().any(|a| a == "--json") {
        print!("{}", json());

        return ExitCode::SUCCESS;
    }

    match topic.map(String::as_str) {
        None => {
            print!("{}", index(color));
            ExitCode::SUCCESS
        }

        Some("lints") => {
            print!("{}", lints_page(color));
            ExitCode::SUCCESS
        }

        Some(t) => match page(t, color) {
            Some(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }

            None => {
                eprintln!("alloy: no doc for `{t}`; `alloy doc` lists the topics");
                ExitCode::FAILURE
            }
        },
    }
}

/// Every entry and every lint as JSON, for the docs site.
fn json() -> String {
    let entries: Vec<serde_json::Value> = TABLE
        .iter()
        .map(|(key, text)| {
            let group = GROUPS
                .iter()
                .find(|(_, pick)| pick(key))
                .map(|(title, _)| *title)
                .unwrap_or("Other");

            serde_json::json!({ "key": key, "group": group, "markdown": text })
        })
        .collect();
    let lints: Vec<serde_json::Value> = LINTS
        .iter()
        .map(|l| {
            let level = match l.default {
                Level::Allow => "strict",
                Level::Warn => "warn",
                Level::Deny => "deny",
            };

            serde_json::json!({ "name": l.name, "group": l.group.name(), "default": level, "summary": l.summary, "detail": l.detail })
        })
        .collect();
    let value = serde_json::json!({
        "version": alloy::VERSION,
        "entries": entries,
        "lints": lints,
    });

    serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
}

/// The page for one topic, or none.
fn page(topic: &str, color: bool) -> Option<String> {
    // A section number, as a diagnostic's code: `alloy doc 4.2`.
    if let Some(sec) = docs::section(topic) {
        let url = docs::book_url(topic).unwrap_or_default();
        let head = format!("**{} {}**\n{url}\n\n", sec.number, sec.title);
        let body = match sec.key {
            Some("lints") => return Some(render(&head, color) + &lints_page(color)),
            Some(key) => docs::lookup(key).unwrap_or(""),
            None => "",
        };

        return Some(render(&format!("{head}{body}"), color));
    }

    if let Some(l) = LINTS.iter().find(|l| l.name == topic) {
        let level = match l.default {
            Level::Allow => "off unless `[lint] strict = true`",
            Level::Warn => "warn",
            Level::Deny => "deny",
        };
        let body = format!(
            "**{}**\nGroup: {}. Default: {level}\n\n{}\n\n{}",
            l.name,
            l.group.name(),
            l.summary,
            l.detail
        );

        return Some(render(&body, color));
    }

    // A group: its lints.
    if let Some(group) = Group::from_name(topic) {
        let mut out = format!("**{}**\n{}\n\n", group.name(), group.summary());

        for l in LINTS.iter().filter(|l| l.group == group) {
            out.push_str(&format!("  {:<24} {}\n", l.name, l.summary));
        }

        out.push_str("\n`alloy doc <lint>` explains one.\n");

        return Some(render(&out, color));
    }

    if topic == alloy::lint::LUAU_GROUP {
        return Some(render(
            "**luau**\nThe type checker's own lints, `LocalUnused`, `LocalShadow`, `ImplicitReturn`, and the rest, which `alloy flux` reports beside Flux's. `[lint]` sets their level by this name, or by the lint's own name.\n",
            color,
        ));
    }

    let keys = [
        topic.to_string(),
        format!("topic:{topic}"),
        format!("@{topic}"),
        format!("${topic}"),
        format!("derive:{topic}"),
    ];

    for key in keys {
        if let Some(text) = docs::lookup(&key) {
            let shown = key.strip_prefix("topic:").unwrap_or(&key);
            let head = if color {
                format!("{BOLD}{shown}{RESET}\n")
            } else {
                format!("{shown}\n")
            };

            return Some(format!("{head}{}", render(text, color)));
        }
    }

    None
}

/// The lint list as a page, by group.
fn lints_page(color: bool) -> String {
    let mut out = String::new();
    out.push_str(&heading("Lints", color));
    out.push_str("`alloy flux` and `alloy lint` run them; `[lint]` in alloy.toml sets `deny`, `warn`, and `allow` lists by lint or by group, and `strict = true` turns the pedantic group on.\n\n");

    for group in Group::ALL {
        out.push_str(&format!("**{}**: {}\n", group.name(), group.summary()));

        for l in LINTS.iter().filter(|l| l.group == *group) {
            let level = match l.default {
                Level::Allow => "allow",
                Level::Warn => "warn",
                Level::Deny => "deny",
            };
            out.push_str(&format!("  {:<24} {:<6} {}\n", l.name, level, l.summary));
        }

        out.push('\n');
    }

    out.push_str("**luau**: the type checker's own lints, under `alloy flux`.\n\n`alloy doc <lint>` explains one; `alloy doc <group>` lists a group.\n");
    render(&out, color)
}

/// The index of every topic.
fn index(color: bool) -> String {
    let mut out = String::new();
    out.push_str(&heading("alloy doc <topic>", color));
    out.push_str("Explains one construct of the language, a lint, or an article. The groups:\n\n");

    let mut taken: Vec<&str> = Vec::new();

    for (title, pick) in GROUPS {
        let mut keys: Vec<&str> = TABLE
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| pick(k) && !taken.contains(k))
            .collect();

        if keys.is_empty() {
            continue;
        }

        keys.sort();
        taken.extend(keys.iter().copied());
        let shown: Vec<String> = keys
            .iter()
            .map(|k| k.strip_prefix("topic:").unwrap_or(k).to_string())
            .collect();
        out.push_str(&heading(title, color));
        out.push_str(&wrap(&shown.join("  "), 4, ui::term_width().clamp(40, 100)));
        out.push('\n');
    }

    out.push_str(&heading("Lints", color));
    let names: Vec<&str> = LINTS.iter().map(|l| l.name).collect();
    out.push_str(&wrap(
        &format!("lints  {}", names.join("  ")),
        4,
        ui::term_width().clamp(40, 100),
    ));
    out.push('\n');
    out
}

fn heading(text: &str, color: bool) -> String {
    if color {
        format!("{BOLD}{text}{RESET}\n")
    } else {
        format!("{text}\n")
    }
}

/// Words wrapped at `width`, each line indented.
fn wrap(text: &str, indent: usize, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();

    for word in text.split("  ") {
        if !line.is_empty() && indent + line.len() + 2 + word.len() > width {
            out.push_str(&" ".repeat(indent));
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }

        if !line.is_empty() {
            line.push_str("  ");
        }

        line.push_str(word);
    }

    if !line.is_empty() {
        out.push_str(&" ".repeat(indent));
        out.push_str(&line);
        out.push('\n');
    }

    out
}

/// Markdown to terminal text: a fenced block indents, inline code and
/// bold take the bold style, and paragraphs wrap.
fn render(markdown: &str, color: bool) -> String {
    let width = ui::term_width().clamp(40, 100);
    let mut out = String::new();
    let mut in_fence = false;
    let mut mode = Mode::Text;

    for line in markdown.lines() {
        if let Some(tag) = line.strip_prefix("```") {
            in_fence = !in_fence;

            if in_fence {
                mode = Mode::of(tag);
            } else {
                out.push('\n');
            }

            continue;
        }

        if in_fence {
            out.push_str("    ");
            out.push_str(&highlight::paint(line, mode, color));
            out.push('\n');

            continue;
        }

        if line.trim().is_empty() {
            out.push('\n');

            continue;
        }

        let styled = inline(line, color);

        if line.starts_with("  ") {
            out.push_str(&styled);
            out.push('\n');
        } else {
            out.push_str(&wrap_prose(&styled, width));
        }
    }

    if !out.ends_with('\n') {
        out.push('\n');
    }

    out
}

/// Inline code and bold markers. A `**` pair bolds; a backtick pair is
/// code, shown in the keyword color; a lone marker stays as it is.
fn inline(line: &str, color: bool) -> String {
    let mut out = String::new();
    let mut rest = line;
    let mut bold_open = false;

    while !rest.is_empty() {
        let tick = rest.find('`');
        let stars = rest.find("**");

        match (tick, stars) {
            (None, None) => {
                out.push_str(rest);

                break;
            }

            (Some(t), s) if s.is_none_or(|s| t < s) => {
                out.push_str(&rest[..t]);
                let after = &rest[t + 1..];

                match after.find('`') {
                    Some(j) => {
                        if color {
                            out.push_str(&format!(
                                "{}{}{RESET}",
                                ui::fg(highlight::KEYWORD),
                                &after[..j]
                            ));

                            if bold_open {
                                out.push_str(BOLD);
                            }
                        } else {
                            out.push_str(&after[..j]);
                        }

                        rest = &after[j + 1..];
                    }

                    None => {
                        out.push('`');
                        rest = after;
                    }
                }
            }

            (_, Some(s)) => {
                out.push_str(&rest[..s]);

                if color {
                    out.push_str(if bold_open { RESET } else { BOLD });
                }

                bold_open = !bold_open;
                rest = &rest[s + 2..];
            }

            (Some(_), None) => unreachable!("a tick with no star is the arm above"),
        }
    }

    out
}

/// Wraps a paragraph at `width` visible columns.
fn wrap_prose(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();

    for word in text.split(' ') {
        let visible = ui::visible_width(&line) + 1 + ui::visible_width(word);

        if !line.is_empty() && visible > width {
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }

        if !line.is_empty() {
            line.push(' ');
        }

        line.push_str(word);
    }

    out.push_str(&line);
    out.push('\n');
    out
}
