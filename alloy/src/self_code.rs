//! `alloy self code`: the editor setup for `alloy.toml`.
//!
//! The schema goes to `~/.alloy/alloy.schema.json`, beside the `bin`
//! directory of the install. Each editor's `settings.json` then gains the
//! association that points its TOML language server at that file. The
//! edits go through `crate::jsonc`, so the comments in the file stay.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::jsonc::{self, Edit};
use crate::ui::{self, Painter};

/// The regex a taplo association matches against the document URI. The
/// backslash is for a URI that carries a Windows path as given.
const TOML_MATCH: &str = r"^(.*[/\\])?alloy\.toml$";

/// The VS Code family: the name, the folder under the user config
/// directory, and the folder under the home directory that holds the
/// installed extensions.
const VSCODE_FAMILY: [(&str, &str, &str); 5] = [
    ("VS Code", "Code", ".vscode"),
    ("VS Code Insiders", "Code - Insiders", ".vscode-insiders"),
    ("VSCodium", "VSCodium", ".vscode-oss"),
    ("Cursor", "Cursor", ".cursor"),
    ("Windsurf", "Windsurf", ".windsurf"),
];

/// The schema file for an install directory: `~/.alloy/alloy.schema.json`
/// for `~/.alloy/bin`, and beside the `bin` of a `--dir` in the same way.
pub fn schema_path(dir: &Path) -> PathBuf {
    dir.parent().unwrap_or(dir).join("alloy.schema.json")
}

/// One editor whose settings file gets the association.
struct Editor {
    name: &'static str,
    settings: PathBuf,
    edits: Vec<Edit>,
    /// The indent for a settings file that has none yet.
    indent: &'static str,
    /// Where the editor keeps its extensions, for the Even Better TOML check.
    extensions: Option<PathBuf>,
}

pub fn run(dir: &Path, dry_run: bool) -> ExitCode {
    run_in(dir, dry_run, dirs::home_dir(), dirs::config_dir())
}

/// The command over a given home and config directory, so a test can
/// point it at a scratch layout.
fn run_in(dir: &Path, dry_run: bool, home: Option<PathBuf>, config: Option<PathBuf>) -> ExitCode {
    let p = Painter::for_stdout();
    let schema = schema_path(dir);

    if let Err(e) = write_schema(&schema, dry_run) {
        eprintln!("{}", Painter::for_stderr().fail(&e));
        return ExitCode::FAILURE;
    }

    println!("{}", p.wrote(&schema.display().to_string()));

    let url = file_url(&schema);
    let editors = editors(&url, home, config);

    if editors.is_empty() {
        println!(
            "{}",
            p.note("no editor settings found; VS Code, VSCodium, Cursor, Windsurf, and Zed are searched")
        );

        return ExitCode::SUCCESS;
    }

    let mut failed = false;

    for editor in &editors {
        if let Err(e) = configure(editor, dry_run, &p) {
            eprintln!("{}", Painter::for_stderr().fail(&e));
            failed = true;
        }
    }

    if dry_run {
        println!("{}", p.note("dry run: nothing was written"));
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn write_schema(path: &Path, dry_run: bool) -> Result<(), String> {
    let text = alloy::schema::to_string();

    if std::fs::read_to_string(path).is_ok_and(|old| old == text) || dry_run {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The `file:` URL of a path, absolute, with the few bytes a URL cannot
/// carry escaped. A Windows path `C:\a b` becomes `file:///C:/a%20b`.
pub fn file_url(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = absolute.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");

    if !text.starts_with('/') {
        url.push('/');
    }

    for c in text.chars() {
        match c {
            ' ' => url.push_str("%20"),
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            _ => url.push(c),
        }
    }

    url
}

/// Every editor whose settings directory exists.
fn editors(url: &str, home: Option<PathBuf>, config: Option<PathBuf>) -> Vec<Editor> {
    let mut out = Vec::new();
    let url_json = serde_json::to_string(url).unwrap_or_default();

    for (name, folder, extensions) in VSCODE_FAMILY {
        let Some(user) = config.as_ref().map(|c| c.join(folder).join("User")) else {
            continue;
        };

        if !user.is_dir() {
            continue;
        }

        // Even Better TOML reads `evenBetterToml.schema.associations`, a
        // regex over the document URI to the schema URL, and VS Code
        // keeps the dotted form as one key.
        out.push(Editor {
            name,
            settings: user.join("settings.json"),
            edits: vec![
                Edit::Set {
                    path: jsonc::path(&["files.associations", "alloy.toml"]),
                    value: "\"toml\"".to_string(),
                },
                Edit::Set {
                    path: jsonc::path(&["evenBetterToml.schema.associations", TOML_MATCH]),
                    value: url_json.clone(),
                },
            ],
            indent: "    ",
            extensions: home.as_ref().map(|h| h.join(extensions).join("extensions")),
        });
    }

    // Zed keeps `~/.config/zed` on Linux and macOS alike; Windows uses
    // the roaming AppData folder.
    let zed = if cfg!(windows) {
        config.as_ref().map(|c| c.join("Zed"))
    } else {
        home.as_ref().map(|h| h.join(".config").join("zed"))
    };

    if let Some(zed) = zed.filter(|z| z.is_dir()) {
        // Zed hands `lsp.<server>.settings` to the server as its
        // workspace configuration, whatever section it asks for, and
        // sends the same object in `workspace/didChangeConfiguration`.
        //
        // taplo asks for the `evenBetterToml` section and merges the
        // object it gets straight into its config (taplo-lsp
        // handlers/configuration.rs, config.rs), so the key is
        // `schema.associations` at the top of the settings object, not
        // under `evenBetterToml`.
        //
        // Zed's TOML extension no longer bundles a server; the Zed docs
        // name the Tombi extension. Tombi reads `settings.tombi` as its
        // own config (tombi-lsp handler/did_change_configuration.rs), so
        // the schema goes into `tombi.schemas` as `{ path, include }`.
        // Tombi reads these editor settings only when no tombi.toml or
        // user config file is found.
        let tombi = format!("{{ \"path\": {url_json}, \"include\": [\"alloy.toml\"] }}");

        out.push(Editor {
            name: "Zed",
            settings: zed.join("settings.json"),
            edits: vec![
                Edit::Set {
                    path: jsonc::path(&[
                        "lsp",
                        "taplo",
                        "settings",
                        "schema",
                        "associations",
                        TOML_MATCH,
                    ]),
                    value: url_json.clone(),
                },
                Edit::Push {
                    path: jsonc::path(&["lsp", "tombi", "settings", "tombi", "schemas"]),
                    value: tombi,
                    marker: url.to_string(),
                },
            ],
            indent: "  ",
            extensions: None,
        });
    }

    out
}

/// Applies the edits of one editor and reports them.
fn configure(editor: &Editor, dry_run: bool, p: &Painter) -> Result<(), String> {
    let path = &editor.settings;
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };

    let new = jsonc::apply(&old, &editor.edits, editor.indent)
        .map_err(|e| format!("{}: {e}", path.display()))?;

    if new == old {
        println!(
            "{}",
            p.note(&format!(
                "{}: already set in {}",
                editor.name,
                path.display()
            ))
        );
    } else {
        if !dry_run {
            std::fs::write(path, &new)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }

        println!("{}", p.ok(&format!("{}: {}", editor.name, path.display())));

        for edit in &editor.edits {
            let (path, value) = match edit {
                Edit::Set { path, value } => (path, value),
                Edit::Push { path, value, .. } => (path, value),
            };

            println!(
                "  {}",
                p.paint(ui::LILAC, &format!("{} = {value}", path.join(".")))
            );
        }
    }

    if let Some(extensions) = &editor.extensions
        && !has_even_better_toml(extensions)
    {
        println!(
            "{}",
            p.warn(&format!(
                "{}: install Even Better TOML (tamasfe.even-better-toml) to complete alloy.toml",
                editor.name
            ))
        );
    }

    Ok(())
}

/// Whether the extensions folder holds a version of Even Better TOML.
fn has_even_better_toml(extensions: &Path) -> bool {
    std::fs::read_dir(extensions)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("tamasfe.even-better-toml")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_sits_beside_bin() {
        assert_eq!(
            schema_path(Path::new("/home/u/.alloy/bin")),
            PathBuf::from("/home/u/.alloy/alloy.schema.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_url_is_absolute_and_escaped() {
        assert_eq!(
            file_url(Path::new("/home/u/my dir/alloy.schema.json")),
            "file:///home/u/my%20dir/alloy.schema.json"
        );
    }

    /// A scratch home with a VS Code settings file that has comments
    /// and a stale association, a Zed file with another server, and no
    /// Even Better TOML.
    fn scratch(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("alloy-self-code-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let config = home.join(".config");
        std::fs::create_dir_all(config.join("Code").join("User")).unwrap();
        std::fs::create_dir_all(config.join("zed")).unwrap();
        std::fs::create_dir_all(
            home.join(".vscode")
                .join("extensions")
                .join("other.ext-1.0"),
        )
        .unwrap();
        std::fs::write(
            config.join("Code").join("User").join("settings.json"),
            "{\n    // my theme\n    \"workbench.colorTheme\": \"Dark\", /* keep */\n    \"files.associations\": {\n        \"*.aly\": \"alloy-luau\",\n        \"alloy.toml\": \"alloy-toml\",\n    },\n}\n",
        )
        .unwrap();
        std::fs::write(
            config.join("zed").join("settings.json"),
            "{\n  \"theme\": \"One Dark\",\n  \"lsp\": {\n    \"rust-analyzer\": { \"binary\": { \"path\": \"ra\" } },\n  },\n}\n",
        )
        .unwrap();
        home
    }

    #[test]
    fn the_command_edits_every_editor_it_finds() {
        let home = scratch("run");
        let config = home.join(".config");
        let dir = home.join(".alloy").join("bin");

        let code = run_in(&dir, false, Some(home.clone()), Some(config.clone()));
        assert_eq!(code, ExitCode::SUCCESS);

        let schema = home.join(".alloy").join("alloy.schema.json");
        assert_eq!(
            std::fs::read_to_string(&schema).unwrap(),
            alloy::schema::to_string()
        );
        let url = file_url(&schema);

        let vscode =
            std::fs::read_to_string(config.join("Code").join("User").join("settings.json"))
                .unwrap();
        assert!(vscode.contains("// my theme"));
        assert!(vscode.contains("\"Dark\", /* keep */"));
        assert!(vscode.contains("\"*.aly\": \"alloy-luau\","));
        assert!(vscode.contains("\"alloy.toml\": \"toml\","));
        assert!(!vscode.contains("alloy-toml"));
        assert!(vscode.contains(&format!(
            "\"evenBetterToml.schema.associations\": {{\n        {}: \"{url}\"\n    }},",
            serde_json::to_string(TOML_MATCH).unwrap()
        )));

        let zed = std::fs::read_to_string(config.join("zed").join("settings.json")).unwrap();
        assert!(zed.contains("\"theme\": \"One Dark\","));
        assert!(zed.contains("\"rust-analyzer\": { \"binary\": { \"path\": \"ra\" } },"));
        assert!(zed.contains("\"taplo\": {\n      \"settings\": {\n        \"schema\": {\n          \"associations\": {"));
        assert!(zed.contains(&format!(
            "\"tombi\": {{\n      \"settings\": {{\n        \"tombi\": {{\n          \"schemas\": [\n            {{ \"path\": \"{url}\", \"include\": [\"alloy.toml\"] }}\n          ]"
        )));

        // A second run changes nothing.
        let again = run_in(&dir, false, Some(home.clone()), Some(config.clone()));
        assert_eq!(again, ExitCode::SUCCESS);
        assert_eq!(
            std::fs::read_to_string(config.join("zed").join("settings.json")).unwrap(),
            zed
        );
        assert_eq!(
            std::fs::read_to_string(config.join("Code").join("User").join("settings.json"))
                .unwrap(),
            vscode
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let home = scratch("dry");
        let config = home.join(".config");
        let dir = home.join(".alloy").join("bin");
        let before = std::fs::read_to_string(config.join("zed").join("settings.json")).unwrap();

        let code = run_in(&dir, true, Some(home.clone()), Some(config.clone()));
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(!home.join(".alloy").exists());
        assert_eq!(
            std::fs::read_to_string(config.join("zed").join("settings.json")).unwrap(),
            before
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_match_is_a_valid_json_key() {
        let key = serde_json::to_string(TOML_MATCH).unwrap();
        assert_eq!(key, "\"^(.*[/\\\\\\\\])?alloy\\\\.toml$\"");
    }
}
