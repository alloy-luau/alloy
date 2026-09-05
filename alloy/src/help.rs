//! The help screen: the logo on the left, the commands on the right.
//! A narrow terminal gets the logo above the text instead.

use crate::art;
use crate::ui::{self, BOLD, RESET};

/// Columns between the logo and the text.
const GAP: usize = 4;

/// The text column. A heading is bold; a line that starts with two
/// spaces is an entry, and the words in `<>` after the name belong to it.
const TEXT: &str = "\
alloy {version}
A strict superset of Luau that compiles to plain Luau.

Usage: alloy <command> [options]

Commands:
  build [file]    Build the project, or one file; -W watches
  check [file]    Compile, write nothing, report errors and lints
  lint [file]     Run the lints of the [lint] table
  fmt [paths]     Format the sources in place
  doc [topic]     Explain a keyword, an operator, a lint, an article
  init            Write alloy.toml, .luaurc, and .config.luau
  self            Install or remove the binaries
  help            Show this screen

Options:
  -h, --help      This screen; after a command, its options
  -V, --version   Print the version
";

/// The help text for `alloy self`, shown without the logo.
pub const SELF_TEXT: &str = "\
Usage: alloy self <command> [--dir <path>]

Commands:
  install         Copy alloy and alloy-lsp to ~/.alloy/bin
  uninstall       Remove the binaries from ~/.alloy/bin

Options:
  --dir <path>    Install to, or remove from, <path>
";

pub const BUILD_TEXT: &str = "\
Usage: alloy build [file] [options]

With no file, builds the project of the nearest alloy.toml. With one,
compiles that .aly, .d.aly, or .alx file to stdout.

Options:
  -W, --watch           Build again after every change, until ctrl-c
  --out <dir>           Write the output under <dir>
  --config <file>       Read this alloy.toml instead of the nearest one
  --check               Emit the check artifact, what luau-lsp sees
  --map                 Print the chunk map of one file
  --wait-timeout <s>    Seconds for every WaitForChild that => emits
";

pub const CHECK_TEXT: &str = "\
Usage: alloy check [file] [options]

Compiles every source, or one file, and writes nothing. Reports the
compiler's diagnostics and the lints at their [lint] levels; exits with
one on any diagnostic or denied lint.

Options:
  --config <file>       Read this alloy.toml instead of the nearest one
  --strict              Turn the strict-only lints on for this run
  --deny-warnings       Fail on any warning
";

pub const LINT_TEXT: &str = "\
Usage: alloy lint [file] [options]

Runs the lints over the project, or one file. `alloy doc lints` names
them; the [lint] table of alloy.toml sets their levels.

Options:
  --config <file>       Read this alloy.toml instead of the nearest one
  --strict              Turn the strict-only lints on for this run
  --deny-warnings       Fail on any warning
  --list                Print every lint with its default level
";

pub const FMT_TEXT: &str = "\
Usage: alloy fmt [paths] [options]

Formats the project's .aly files in place, or the paths given. Only
whitespace changes.

Options:
  --check               Write nothing; fail when a file would change
  --config <file>       Read this alloy.toml instead of the nearest one
";

pub const DOC_TEXT: &str = "\
Usage: alloy doc [topic] [options]

Prints one entry: a keyword, an operator, an intrinsic, an attribute, a
std name, a lint, or an article such as `strict`. With no topic, lists
them all.

Options:
  --json                Print every entry and lint as JSON
";

/// Renders the full help screen.
pub fn render(color: bool) -> String {
    let text = TEXT.replace("{version}", alloy::VERSION);
    let text_lines: Vec<String> = text.lines().map(|l| style_line(l, color)).collect();

    let logo = art::logo(color);
    let logo_lines: Vec<&str> = logo.lines().collect();
    let logo_w = logo_lines
        .iter()
        .map(|l| ui::visible_width(l))
        .max()
        .unwrap_or(0);

    let text_w = text_lines
        .iter()
        .map(|l| ui::visible_width(l))
        .max()
        .unwrap_or(0);

    let side_by_side = ui::term_width() >= logo_w + GAP + text_w;
    let mut out = String::new();

    if side_by_side {
        let rows = logo_lines.len().max(text_lines.len());
        let logo_off = (rows - logo_lines.len()) / 2;
        let text_off = (rows - text_lines.len()) / 2;

        for row in 0..rows {
            let logo_line = row
                .checked_sub(logo_off)
                .and_then(|i| logo_lines.get(i).copied())
                .unwrap_or("");

            let text_line = row
                .checked_sub(text_off)
                .and_then(|i| text_lines.get(i))
                .map(String::as_str)
                .unwrap_or("");

            out.push_str(logo_line);

            for _ in 0..logo_w - ui::visible_width(logo_line) + GAP {
                out.push(' ');
            }

            out.push_str(&tint(text_line, row, rows, color));

            while out.ends_with(' ') {
                out.pop();
            }

            out.push('\n');
        }
    } else {
        out.push_str(&logo);
        out.push_str("\n\n");

        let rows = text_lines.len();

        for (row, line) in text_lines.iter().enumerate() {
            out.push_str(&tint(line, row, rows, color));
            out.push('\n');
        }
    }

    out
}

/// Renders a help text that has no logo, such as `alloy self`.
pub fn render_plain(text: &str, color: bool) -> String {
    let lines: Vec<String> = text.lines().map(|l| style_line(l, color)).collect();
    let rows = lines.len();
    let mut out = String::new();

    for (row, line) in lines.iter().enumerate() {
        out.push_str(&tint(line, row, rows, color));
        out.push('\n');
    }

    out
}

/// Makes a heading bold, and the name of an entry bold.
fn style_line(line: &str, color: bool) -> String {
    if !color || line.is_empty() {
        return line.to_owned();
    }

    if let Some(rest) = line.strip_prefix("  ") {
        // The name ends at the first run of two spaces.
        let split = rest.find("  ").unwrap_or(rest.len());
        let (name, desc) = rest.split_at(split);

        return format!("  {BOLD}{name}{RESET}{desc}");
    }

    if line.ends_with(':') || line.starts_with("alloy ") {
        return format!("{BOLD}{line}{RESET}");
    }

    line.to_owned()
}

/// Paints one text row in the gradient color of its row. The color is
/// applied again after every reset, so a bold span keeps the color.
fn tint(line: &str, row: usize, rows: usize, color: bool) -> String {
    if !color || line.is_empty() {
        return line.to_owned();
    }

    let c = ui::fg(art::row_color(row, rows));
    let reapplied = line.replace(RESET, &format!("{RESET}{c}"));

    format!("{c}{reapplied}{RESET}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_render_lists_every_command() {
        let help = render(false);

        for name in ["build", "check", "lint", "fmt", "doc", "init", "self"] {
            assert!(help.contains(name), "{name} is missing");
        }

        assert!(!help.contains('\x1b'));
        assert!(help.contains(alloy::VERSION));
    }

    #[test]
    fn entry_name_is_bold() {
        let line = style_line("  build <file>          Compile", true);
        assert_eq!(line, "  \x1b[1mbuild <file>\x1b[0m          Compile");
    }
}
