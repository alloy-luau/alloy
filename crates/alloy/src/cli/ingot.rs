//! `alloy ingot <command>`: install the ingots a project declares, and
//! scaffold, inspect, and run one without a project, so an author sees
//! what the host sees.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alloy::config::{Config, IngotSource};
use alloy::ingot::fetch::{self, Outcome};
use alloy::ingot::manifest::{self, Manifest};
use alloy::ingot::{Hook, Ingots};

use crate::help;
use crate::ui::{self, Painter};

fn fail(message: &str) {
    eprintln!("{}", Painter::for_stderr().fail(message));
}

pub(crate) fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("new") => match args.get(1) {
            Some(name) => new(name),

            None => {
                fail("`alloy ingot new` needs a name");
                ExitCode::FAILURE
            }
        },

        Some("info") => match args.get(1) {
            Some(dir) => info(&dir_of(dir)),

            None => {
                fail("`alloy ingot info` needs a directory");
                ExitCode::FAILURE
            }
        },

        Some("install") => install(args.get(1).map(String::as_str), false),

        Some("update") => install(args.get(1).map(String::as_str), true),

        Some("run") => match (args.get(1), args.get(2)) {
            (Some(dir), Some(file)) => run_one(&dir_of(dir), Path::new(file), &args[3..]),

            _ => {
                fail("`alloy ingot run` needs a directory and a file");
                ExitCode::FAILURE
            }
        },

        Some("--help" | "-h" | "help") | None => {
            print!("{}", help::render_plain(help::INGOT_TEXT, ui::want_color()));
            ExitCode::SUCCESS
        }

        Some(other) => {
            fail(&format!("unknown ingot command `{other}`"));
            eprint!("{}", help::render_plain(help::INGOT_TEXT, false));
            ExitCode::FAILURE
        }
    }
}

/// `alloy ingot install [name]` and `alloy ingot update [name]`.
///
/// The install fetches what the project declares and does not have.
/// The update, `refresh`, asks each repository for its latest release
/// again; a pinned version stays where it is and says so.
fn install(only: Option<&str>, refresh: bool) -> ExitCode {
    let p = Painter::for_stdout();
    let word = if refresh { "update" } else { "install" };
    let Some(config_path) = Config::find(Path::new(".")) else {
        fail(&format!(
            "`alloy ingot {word}` needs an alloy.toml or a .config.aly; neither is here or above"
        ));

        return ExitCode::FAILURE;
    };
    let config = match Config::load(&config_path) {
        Ok(c) => c,

        Err(e) => {
            fail(&format!("{}: {e}", config_path.display()));

            return ExitCode::FAILURE;
        }
    };
    let root = config_path.parent().unwrap_or(Path::new("."));
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());

    if let Some(name) = only
        && !config.ingots.contains_key(name)
    {
        fail(&format!(
            "{} declares no ingot `{name}`",
            config_path.display()
        ));

        return ExitCode::FAILURE;
    }

    if config.ingots.is_empty() {
        println!(
            "{}",
            p.note(&format!("{} declares no ingots", config_path.display()))
        );

        return ExitCode::SUCCESS;
    }

    let mut failed = false;

    for (name, source) in &config.ingots {
        if only.is_some_and(|want| want != name) {
            continue;
        }

        let table = source.table();

        match fetch::install(&root, name, &table, &fetch::GitHub, refresh) {
            Ok(Outcome::Local) => println!(
                "{}",
                p.note(&format!(
                    "{name} is a path in this project; nothing to fetch"
                ))
            ),

            Ok(Outcome::Present(version)) => println!(
                "{}",
                p.note(&format!(
                    "{name} {version} is installed{}",
                    if refresh { " and is the latest" } else { "" }
                ))
            ),

            Ok(Outcome::Pinned(version)) => println!(
                "{}",
                p.note(&format!(
                    "{name} is pinned at {version}; the update leaves it. Set `version = \"^\"` to follow the latest release"
                ))
            ),

            Ok(Outcome::Fetched(version)) => println!(
                "{}",
                p.ok(&format!(
                    "{name} {version} → {}",
                    fetch::store(&root, name, &version).display()
                ))
            ),

            Err(e) => {
                fail(&format!("ingot `{name}`: {e}"));
                failed = true;
            }
        }
    }

    if failed {
        return ExitCode::FAILURE;
    }

    println!(
        "{}",
        p.note(&format!(
            "{} records what is installed",
            fetch::lock_path(&root).display()
        ))
    );

    ExitCode::SUCCESS
}

/// A Rust project that depends on `alloy-ingot`, with a manifest and a
/// handler that transforms, lints, and answers hover.
fn new(name: &str) -> ExitCode {
    let p = Painter::for_stdout();
    let dir = PathBuf::from(name);

    if dir.exists() {
        fail(&format!("{name} exists already"));

        return ExitCode::FAILURE;
    }

    let crate_name = format!("{name}-ingot");
    let files: Vec<(PathBuf, String)> = vec![
        (
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"An Alloy ingot.\"\n\n[dependencies]\nalloy-ingot = \"{}\"\n\n[profile.release]\nstrip = true\n",
                alloy::VERSION
            ),
        ),
        (dir.join("ingot.toml"), manifest::template(name)),
        (dir.join("src").join("main.rs"), main_template(name)),
        (dir.join(".gitignore"), "target/\n".to_string()),
        (
            dir.join("README.md"),
            format!(
                "# {name}\n\nAn Alloy ingot. Build it with `cargo build --release`, then add it to a project:\n\n```toml\n[ingots]\n{name} = \"path/to/{name}\"\n```\n\n`alloy ingot run . file.aly` pushes one file through it.\n"
            ),
        ),
    ];

    for (path, text) in files {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                fail(&format!("cannot create {}: {e}", parent.display()));

                return ExitCode::FAILURE;
            }
        }

        if let Err(e) = std::fs::write(&path, text) {
            fail(&format!("cannot write {}: {e}", path.display()));

            return ExitCode::FAILURE;
        }

        println!("{}", p.wrote(&path.display().to_string()));
    }

    println!(
        "{}",
        p.note(&format!(
            "cd {name} && cargo build --release, then `alloy ingot run . <file>` to try it"
        ))
    );

    ExitCode::SUCCESS
}

fn main_template(name: &str) -> String {
    format!(
        r#"//! The {name} ingot. `alloy doc ingots` explains the hooks; the
//! `alloy-ingot` crate documents each type.

use alloy_ingot::{{Edit, File, Finding, Handler, Hover, Settings, serve}};

#[derive(Default)]
struct Ingot {{
    greeting: String,
}}

impl Handler for Ingot {{
    fn init(&mut self, settings: &Settings) -> Result<(), String> {{
        self.greeting = settings.options["greeting"]
            .as_str()
            .unwrap_or("hello")
            .to_string();

        Ok(())
    }}

    /// Edits to the Alloy source, before the desugar. The line count
    /// must hold: replace text on its own line.
    fn transform(&mut self, file: &File) -> Result<Vec<Edit>, String> {{
        let _ = file;

        Ok(Vec::new())
    }}

    fn lint(&mut self, file: &File) -> Result<Vec<Finding>, String> {{
        let mut findings = Vec::new();

        for (at, _) in file.source.match_indices("TODO") {{
            findings.push(Finding::new(
                "example_lint",
                (at as u32, at as u32 + 4),
                "a TODO the scaffold flags; replace this lint",
            ));
        }}

        Ok(findings)
    }}

    fn hover(&mut self, file: &File, offset: u32) -> Result<Option<Hover>, String> {{
        Ok(file
            .word_at(offset)
            .filter(|(w, _)| *w == "TODO")
            .map(|(_, span)| Hover::new(format!("{{}} from the {name} ingot", self.greeting)).over(span)))
    }}

    fn manifest(&self) -> Option<&'static str> {{
        Some(include_str!("../ingot.toml"))
    }}
}}

fn main() {{
    serve(Ingot::default())
}}
"#
    )
}

/// What a manifest declares, without starting the binary.
/// The directory an argument names: a path that holds `ingot.toml`, or
/// the name of an ingot under `[ingots]` in the nearest alloy.toml.
fn dir_of(arg: &str) -> PathBuf {
    let path = PathBuf::from(arg);

    if path.join(manifest::FILE_NAME).is_file() {
        return path;
    }

    let Some(config_path) = alloy::config::Config::find(Path::new(".")) else {
        return path;
    };
    let Ok(config) = alloy::config::Config::load(&config_path) else {
        return path;
    };
    let root = config_path.parent().unwrap_or(Path::new("."));

    match config.ingots.get(arg) {
        Some(source) => {
            let table = source.table();

            match &table.path {
                Some(p) => root.join(p),

                // An installed ingot sits in the store, at the version
                // the lock file names.
                None => fetch::resolve(root, arg, &table).unwrap_or(path),
            }
        }

        None => path,
    }
}

fn info(dir: &Path) -> ExitCode {
    let p = Painter::for_stdout();
    let manifest = match Manifest::load(&dir.join(manifest::FILE_NAME)) {
        Ok(m) => m,

        Err(e) => {
            fail(&e);

            return ExitCode::FAILURE;
        }
    };
    let binary = alloy::ingot::find_binary(dir, &manifest.binary_name());

    println!(
        "{} {}",
        p.bold(&manifest.name),
        p.paint(ui::DIM, &manifest.description)
    );
    println!("  api        {}", manifest.api);
    println!(
        "  binary     {}",
        match &binary {
            Some(b) => b.display().to_string(),

            None => format!("{} (not built)", manifest.binary_name()),
        }
    );
    println!(
        "  hooks      {}",
        manifest
            .hooks
            .iter()
            .map(|h| h.name())
            .collect::<Vec<_>>()
            .join(", ")
    );

    if let Some(run) = manifest.run {
        println!("  run        {}", run.order());
    }

    if !manifest.kinds.is_empty() {
        println!("  kinds      {}", manifest.kinds.join(", "));
    }

    for (k, v) in &manifest.options {
        println!("  option     {k} = {v}");
    }

    for (name, l) in &manifest.lints {
        println!("  lint       {name} ({}) {}", l.default, l.summary);
    }

    for (name, prop) in &manifest.props {
        println!("  prop       {name} {}", prop.doc());
    }

    ExitCode::SUCCESS
}

/// One file through one ingot, with a throwaway config that names it.
fn run_one(dir: &Path, file: &Path, args: &[String]) -> ExitCode {
    let p = Painter::for_stderr();
    let dir = match dir.canonicalize() {
        Ok(d) => d,

        Err(e) => {
            fail(&format!("{}: {e}", dir.display()));

            return ExitCode::FAILURE;
        }
    };
    let manifest = match Manifest::load(&dir.join(manifest::FILE_NAME)) {
        Ok(m) => m,

        Err(e) => {
            fail(&e);

            return ExitCode::FAILURE;
        }
    };
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,

        Err(e) => {
            fail(&format!("{}: {e}", file.display()));

            return ExitCode::FAILURE;
        }
    };
    let mut config = Config::default();
    config.ingots.insert(
        manifest.name.clone(),
        IngotSource::Path(dir.to_string_lossy().into_owned()),
    );
    let root = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let ingots = Ingots::load(&root, &config);

    for problem in &ingots.problems {
        fail(&problem.to_string());
    }

    if ingots.is_empty() {
        return ExitCode::FAILURE;
    }

    let path = file.to_string_lossy().into_owned();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u32>().ok())
    };

    if let Some(offset) = value("--hover") {
        match ingots.hover(&path, &source, offset) {
            Some(h) => println!("{}", h["contents"].as_str().unwrap_or("")),

            None => eprintln!("{}", p.note("no hover")),
        }

        return ExitCode::SUCCESS;
    }

    if let Some(offset) = value("--complete") {
        let (items, _) = ingots.complete(&path, &source, offset, None);

        for item in items {
            println!(
                "{}  {}",
                item["label"].as_str().unwrap_or(""),
                item["detail"].as_str().unwrap_or("")
            );
        }

        return ExitCode::SUCCESS;
    }

    if flag("--format") {
        let (text, problems) = ingots.format(&path, &source);

        for problem in problems {
            fail(&problem.to_string());
        }

        print!("{text}");

        return ExitCode::SUCCESS;
    }

    let options = alloy::EmitOptions {
        file_name: path.clone(),
        definitions: path.ends_with(".d.aly"),
        ..alloy::EmitOptions::default()
    };
    let out = match alloy::compile_file(&path, &source, &options, None, Some(&ingots)) {
        Ok(o) => o,

        Err(e) => {
            fail(&format!("{path}: {e}"));

            return ExitCode::FAILURE;
        }
    };

    for d in &out.diagnostics {
        eprintln!("{}", p.fail(&d.message));
    }

    if flag("--lint") {
        for l in &out.lints {
            let line = source[..l.start as usize].matches('\n').count() + 1;
            println!("{}:{line}: {}", l.name, l.message);
        }

        return ExitCode::SUCCESS;
    }

    // The transform's own result is the source after the edits; the
    // output hook's is the ship artifact.
    let text = if flag("--output") {
        out.ship.clone()
    } else {
        ingots.before(&path, &source).text
    };
    let lines_in = source.lines().count();
    let lines_out = text.lines().count();

    if lines_in == lines_out {
        eprintln!(
            "{}",
            p.note(&format!("{lines_in} lines in, {lines_out} lines out"))
        );
    } else {
        fail(&format!(
            "line count changed, {lines_in} in and {lines_out} out; the map cannot follow"
        ));
    }

    print!("{text}");

    let _ = manifest.has(Hook::Transform);

    ExitCode::SUCCESS
}
