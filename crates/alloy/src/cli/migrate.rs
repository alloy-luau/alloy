//! `alloy migrate`: rewrites `alloy.toml` as `.config.aly`.

use std::path::Path;
use std::process::ExitCode;

use alloy::{config, config_aly};

use crate::fail;
use crate::ui::Painter;

/// Writes `.config.aly` from `alloy.toml` in the current folder, then
/// removes `alloy.toml`, which would win while it stays. The new file
/// must load to the same config first, so a failed rewrite changes
/// nothing.
pub(crate) fn migrate(_args: &[String]) -> ExitCode {
    let p = Painter::for_stdout();
    let (from, to) = (
        Path::new(config::FILE_NAME),
        Path::new(config_aly::FILE_NAME),
    );

    if to.exists() {
        fail(&format!("{} already exists", to.display()));

        return ExitCode::FAILURE;
    }

    let written = std::fs::read_to_string(from)
        .map_err(|e| format!("cannot read {}: {e}", from.display()))
        .and_then(|text| config_aly::from_toml(&text))
        .and_then(|source| {
            std::fs::write(to, source).map_err(|e| format!("cannot write {}: {e}", to.display()))
        });

    if let Err(e) = written {
        fail(&e);

        return ExitCode::FAILURE;
    }

    println!("{}", p.wrote(&to.display().to_string()));

    if let Err(e) = std::fs::remove_file(from) {
        fail(&format!(
            "cannot remove {}: {e}; it wins over {} until it goes",
            from.display(),
            to.display()
        ));

        return ExitCode::FAILURE;
    }

    println!("{}", p.ok(&format!("removed {}", from.display())));

    ExitCode::SUCCESS
}
