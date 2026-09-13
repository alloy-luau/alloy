//! `alloy` command line entry point.

use std::process::ExitCode;

mod art;
mod cli;
mod help;
mod highlight;
mod self_code;
mod ui;

use ui::Painter;

/// The version of this binary, for `alloy self update`.
pub fn alloy_version() -> &'static str {
    alloy::VERSION
}

/// A wrong invocation points at the help screen and fails.
pub(crate) fn usage() -> ExitCode {
    let p = Painter::for_stderr();
    eprintln!(
        "{}",
        p.note("usage: alloy <command> [options]; `alloy --help` lists the commands")
    );
    ExitCode::FAILURE
}

/// `✗ message` on stderr.
pub(crate) fn fail(message: &str) {
    eprintln!("{}", Painter::for_stderr().fail(message));
}

/// `alloy <command> --help` prints that command's options.
fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

fn command_help(text: &str) -> ExitCode {
    print!("{}", help::render_plain(text, ui::want_color()));
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("alloy {}", alloy::VERSION);
            ExitCode::SUCCESS
        }

        Some("--help" | "-h" | "help") | None => {
            print!("{}", help::render(ui::want_color()));
            ExitCode::SUCCESS
        }

        Some("build") if wants_help(&args) => command_help(help::BUILD_TEXT),

        Some("build") => cli::build::build(&args[1..]),

        Some("check") if wants_help(&args) => command_help(help::CHECK_TEXT),

        Some("check") => cli::check::check(&args[1..]),

        Some("lint") if wants_help(&args) => command_help(help::LINT_TEXT),

        Some("lint") => cli::lint::lint_cmd(&args[1..]),

        Some("flux") if wants_help(&args) => command_help(help::FLUX_TEXT),

        Some("flux") => cli::flux::flux_cmd(&args[1..]),

        Some("test") if wants_help(&args) => command_help(help::TEST_TEXT),

        Some("test") => cli::test::test_cmd(&args[1..]),

        Some("fmt") if wants_help(&args) => command_help(help::FMT_TEXT),

        Some("fmt") => cli::fmt::fmt_cmd(&args[1..]),

        Some("doc") if wants_help(&args) => command_help(help::DOC_TEXT),

        Some("doc") => cli::doc::run(&args[1..]),

        Some("init") if wants_help(&args) => command_help(help::INIT_TEXT),
        Some("init") => cli::init::init(),

        Some("self") => cli::self_cmd::run(&args[1..]),

        Some("ingot") if wants_help(&args) => command_help(help::INGOT_TEXT),

        Some("ingot") => cli::ingot::run(&args[1..]),

        Some(other) => {
            fail(&format!("unknown command `{other}`"));
            usage()
        }
    }
}
