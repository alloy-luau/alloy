//! The one place Alloy reaches the network: `alloy self update` and the
//! ingot store.
//!
//! No HTTP client is linked in. A tool the machine already carries does
//! the fetch: curl, then wget, then PowerShell's `Invoke-WebRequest`,
//! which every Windows has. The first one that answers wins, and the
//! answer is remembered for the process.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// What GitHub's API wants on a request.
const ACCEPT: &str = "application/vnd.github+json";
const AGENT: &str = "alloy";

/// The downloader in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Curl,
    Wget,
    /// `Invoke-WebRequest`. The string is the executable: `powershell`
    /// on every Windows, `pwsh` where only the new one is installed.
    PowerShell(&'static str),
}

impl Tool {
    pub fn program(self) -> &'static str {
        match self {
            Tool::Curl => "curl",
            Tool::Wget => "wget",
            Tool::PowerShell(exe) => exe,
        }
    }
}

/// What to say when the machine carries none of them.
pub fn missing() -> String {
    if cfg!(windows) {
        "no way to download: alloy needs curl, wget, or PowerShell on PATH".to_string()
    } else {
        "no way to download: alloy needs curl or wget on PATH".to_string()
    }
}

/// Whether a program answers `--version`.
fn present(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The downloader this machine has, probed once.
pub fn tool() -> Option<Tool> {
    static FOUND: OnceLock<Option<Tool>> = OnceLock::new();

    *FOUND.get_or_init(|| {
        if present("curl", &["--version"]) {
            return Some(Tool::Curl);
        }

        if present("wget", &["--version"]) {
            return Some(Tool::Wget);
        }

        for exe in ["powershell", "pwsh"] {
            if present(
                exe,
                &["-NoProfile", "-Command", "$PSVersionTable.PSVersion.Major"],
            ) {
                return Some(Tool::PowerShell(exe));
            }
        }

        None
    })
}

/// A PowerShell single-quoted string: the quote doubles inside it.
fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn ran(mut command: Command, what: &str) -> Result<std::process::Output, String> {
    let out = command
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;

    if out.status.success() {
        return Ok(out);
    }

    let why = String::from_utf8_lossy(&out.stderr).trim().to_string();

    Err(if why.is_empty() {
        format!("{what} failed")
    } else {
        why
    })
}

/// The body of a URL as text, with GitHub's headers.
pub fn get(url: &str) -> Result<String, String> {
    let tool = tool().ok_or_else(missing)?;
    let mut command = Command::new(tool.program());

    match tool {
        Tool::Curl => {
            command.args(["-fsSL", "-H"]);
            command.arg(format!("Accept: {ACCEPT}"));
            command.arg("-H");
            command.arg(format!("User-Agent: {AGENT}"));
            command.arg(url);
        }

        Tool::Wget => {
            command.arg("-qO-");
            command.arg(format!("--header=Accept: {ACCEPT}"));
            command.arg(format!("--header=User-Agent: {AGENT}"));
            command.arg(url);
        }

        Tool::PowerShell(_) => {
            command.args(["-NoProfile", "-Command"]);
            command.arg(format!(
                "$ProgressPreference='SilentlyContinue'; \
                 (Invoke-WebRequest -Uri {} -UseBasicParsing \
                 -Headers @{{'Accept'={}; 'User-Agent'={}}}).Content",
                ps_quote(url),
                ps_quote(ACCEPT),
                ps_quote(AGENT),
            ));
        }
    }

    let out = ran(command, tool.program())?;

    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A URL into a file. The byte count of the file comes back.
pub fn download(url: &str, to: &Path) -> Result<u64, String> {
    let tool = tool().ok_or_else(missing)?;
    let mut command = Command::new(tool.program());

    match tool {
        Tool::Curl => {
            command.args(["-fsSL", "-o"]);
            command.arg(to);
            command.arg(url);
        }

        Tool::Wget => {
            command.arg("-qO");
            command.arg(to);
            command.arg(url);
        }

        Tool::PowerShell(_) => {
            command.args(["-NoProfile", "-Command"]);
            command.arg(format!(
                "$ProgressPreference='SilentlyContinue'; \
                 Invoke-WebRequest -Uri {} -OutFile {} -UseBasicParsing \
                 -Headers @{{'User-Agent'={}}}",
                ps_quote(url),
                ps_quote(&to.to_string_lossy()),
                ps_quote(AGENT),
            ));
        }
    }

    ran(command, tool.program())?;

    let size = std::fs::metadata(to)
        .map_err(|e| format!("the download wrote no file: {e}"))?
        .len();

    Ok(size)
}

/// Holds a downloaded file to the size the release reports. A proxy
/// that answers an error page with status 200 lands here, not in the
/// install directory.
pub fn check_size(got: u64, expected: u64) -> Result<(), String> {
    if expected == 0 || got >= expected {
        return Ok(());
    }

    Err(format!(
        "the download is {got} bytes and the release says {expected}; it is cut short"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_powershell_string_doubles_its_quotes() {
        assert_eq!(ps_quote("a'b"), "'a''b'");
        assert_eq!(ps_quote("https://x/y"), "'https://x/y'");
    }

    #[test]
    fn a_short_download_is_refused() {
        assert!(check_size(10, 20).is_err());
        assert!(check_size(20, 20).is_ok());
        // A release that reports no size cannot be checked.
        assert!(check_size(1, 0).is_ok());
    }

    /// curl and wget both read `file://`, so the helper can be driven
    /// end to end with no network. PowerShell reads none, and this
    /// machine may carry nothing at all; the test then says nothing.
    #[cfg(unix)]
    #[test]
    fn the_helper_fetches_a_file_url() {
        if !matches!(tool(), Some(Tool::Curl) | Some(Tool::Wget)) {
            eprintln!("no curl or wget; the fetch helper is not checked");

            return;
        }

        let dir = std::env::temp_dir().join(format!("alloy-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("release.json");
        std::fs::write(&source, "{\"tag_name\": \"v0.1.0-rc\"}").unwrap();
        let url = format!("file://{}", source.display());

        assert!(get(&url).unwrap().contains("v0.1.0-rc"));

        let copy = dir.join("copy.json");
        let size = download(&url, &copy).unwrap();
        assert_eq!(size, std::fs::metadata(&source).unwrap().len());
        assert!(check_size(size, size).is_ok());
        assert!(get(&format!("file://{}", dir.join("missing").display())).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_missing_note_names_the_tools() {
        let note = missing();
        assert!(note.contains("curl"));
        assert!(note.contains("wget"));
    }
}
