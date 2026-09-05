//! Log levels for stderr. The editor shows the server's stderr in its
//! output channel, so the level decides what a user sees there.
//!
//! The level is a process wide atomic. The server logs from two threads
//! and from code that runs before the server exists, so a field on the
//! server would not reach every site.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    /// What a user sees without a setting: problems, and nothing else.
    pub const DEFAULT: Level = Level::Warn;

    pub const NAMES: [&'static str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

    /// Parses a level name. The match ignores case.
    pub fn parse(name: &str) -> Option<Level> {
        match name.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Level::DEFAULT as u8);

pub fn set(level: Level) {
    CURRENT.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> Level {
    match CURRENT.load(Ordering::Relaxed) {
        0 => Level::Off,
        1 => Level::Error,
        2 => Level::Warn,
        3 => Level::Info,
        4 => Level::Debug,
        _ => Level::Trace,
    }
}

/// Writes one line to stderr when the level is on.
pub fn log(at: Level, text: &str) {
    if at != Level::Off && at <= level() {
        eprintln!("alloy-lsp [{}] {text}", at.name());
    }
}

pub fn error(text: &str) {
    log(Level::Error, text);
}

pub fn warn(text: &str) {
    log(Level::Warn, text);
}

pub fn info(text: &str) {
    log(Level::Info, text);
}

pub fn debug(text: &str) {
    log(Level::Debug, text);
}

pub fn trace(text: &str) {
    log(Level::Trace, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_every_name() {
        for (i, name) in Level::NAMES.iter().enumerate() {
            assert_eq!(Level::parse(name).map(|l| l as usize), Some(i));
        }

        assert_eq!(Level::parse("WARNING"), Some(Level::Warn));
        assert_eq!(Level::parse("loud"), None);
    }

    #[test]
    fn levels_order() {
        assert!(Level::Off < Level::Error);
        assert!(Level::Error < Level::Trace);
    }
}
