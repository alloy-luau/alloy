//! Terminal styling. The brand color is defined only here.

use std::io::IsTerminal;

/// The brand color of Alloy, #7A58E0, sampled from assets/aly-symbol.png.
pub const BRAND: (u8, u8, u8) = (0x7A, 0x58, 0xE0);

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// The ANSI truecolor foreground escape for an RGB triple.
pub fn fg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

/// Use color only for a real terminal, and obey NO_COLOR.
pub fn want_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// The same test for stderr, where reports go.
pub fn want_color_err() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

/// The status colors, from the website's palette.
pub const GREEN: (u8, u8, u8) = (0x9F, 0xE2, 0xC0);
pub const RED: (u8, u8, u8) = (0xF0, 0x7A, 0x7A);
pub const AMBER: (u8, u8, u8) = (0xFF, 0xD0, 0x8A);
pub const DIM: (u8, u8, u8) = (0x8C, 0x84, 0xA8);
pub const LILAC: (u8, u8, u8) = (0xC9, 0xB5, 0xFF);

/// Styled lines for the reports. With color off, a glyph becomes a
/// word, so a log file and a script read the same thing.
#[derive(Debug, Clone, Copy)]
pub struct Painter {
    pub color: bool,
}

impl Painter {
    pub fn for_stdout() -> Self {
        Self {
            color: want_color(),
        }
    }

    pub fn for_stderr() -> Self {
        Self {
            color: want_color_err(),
        }
    }

    fn mark(&self, glyph: &str, word: &str, rgb: (u8, u8, u8)) -> String {
        if self.color {
            format!("{}{glyph}{RESET}", fg(rgb))
        } else {
            word.to_string()
        }
    }

    pub fn paint(&self, rgb: (u8, u8, u8), text: &str) -> String {
        if self.color {
            format!("{}{text}{RESET}", fg(rgb))
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        if self.color {
            format!("{BOLD}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// `✓ message`, in the brand purple.
    pub fn ok(&self, message: &str) -> String {
        format!("{} {message}", self.mark("✓", "ok:", LILAC))
    }

    /// `✗ message`.
    pub fn fail(&self, message: &str) -> String {
        format!("{} {message}", self.mark("✗", "error:", RED))
    }

    /// `! message`.
    pub fn warn(&self, message: &str) -> String {
        format!("{} {message}", self.mark("!", "warning:", AMBER))
    }

    /// `· message`, in the dim color.
    pub fn note(&self, message: &str) -> String {
        format!(
            "{} {}",
            self.mark("·", "note:", DIM),
            self.paint(DIM, message)
        )
    }

    /// `→ message`, for a file written or a path.
    pub fn wrote(&self, message: &str) -> String {
        format!("{} {message}", self.mark("→", "wrote:", LILAC))
    }

    /// A file location: `path:line:col`, the path dim and the numbers plain.
    pub fn at(&self, path: &str, line: usize, col: usize) -> String {
        if self.color {
            format!("{}{path}{RESET}:{line}:{col}", fg(DIM))
        } else {
            format!("{path}:{line}:{col}")
        }
    }

    /// One diagnostic: the location, a level label, the message.
    pub fn diagnostic(
        &self,
        path: &str,
        line: usize,
        col: usize,
        level: Level,
        code: Option<&str>,
        message: &str,
    ) -> String {
        let (glyph, word, rgb) = match level {
            Level::Error => ("✗", "error", RED),
            Level::Warning => ("!", "warning", AMBER),
        };
        let label = match code {
            Some(c) => format!("{word}[{c}]"),
            None => word.to_string(),
        };
        let label = if self.color {
            format!("{}{glyph} {label}{RESET}", fg(rgb))
        } else {
            label
        };

        format!("{} {label}: {message}", self.at(path, line, col))
    }

    /// The closing line of a run: counts, each with its own color.
    pub fn summary(&self, parts: &[(usize, &str, (u8, u8, u8))]) -> String {
        let shown: Vec<String> = parts
            .iter()
            .map(|(n, what, rgb)| {
                let text = format!("{n} {what}");

                if *n > 0 && self.color {
                    format!("{}{text}{RESET}", fg(*rgb))
                } else {
                    self.paint(DIM, &text)
                }
            })
            .collect();
        let sep = if self.color {
            self.paint(DIM, " · ")
        } else {
            ", ".to_string()
        };

        shown.join(&sep)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

/// The terminal width in columns. COLUMNS wins, then the tty, then 100.
pub fn term_width() -> usize {
    if let Some(cols) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return cols;
    }

    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(100)
}

/// The width of a line on screen. Escape sequences take no columns.
pub fn visible_width(line: &str) -> usize {
    let mut width = 0;
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }

    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_ignores_ansi() {
        assert_eq!(visible_width("abc"), 3);
        assert_eq!(visible_width("\x1b[38;2;1;2;3mab\x1b[0mc"), 3);
        assert_eq!(visible_width(""), 0);
    }
}
