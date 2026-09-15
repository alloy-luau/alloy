//! One running ingot: the child process and the framed pipe to it.
//!
//! Requests go one at a time. A reader thread turns the child's stdout
//! into frames on a channel, so a request can wait with a timeout: a
//! hung ingot costs one request, never the editor. After a timeout or a
//! broken pipe the process is dead, and every later request answers
//! with the same error until a reload.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::Value;

/// How long a request may take before the ingot counts as hung.
pub const TIMEOUT: Duration = Duration::from_secs(20);

pub struct Process {
    inner: Mutex<Inner>,
}

struct Inner {
    child: Option<Child>,
    stdin: Option<std::process::ChildStdin>,
    frames: Option<Receiver<Vec<u8>>>,
    /// Why the process is gone, once it is.
    dead: Option<String>,
}

impl Process {
    /// Starts the binary with the working directory of its manifest.
    pub fn start(binary: &Path, dir: &Path) -> Result<Process, String> {
        let mut child = Command::new(binary)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = channel();

        std::thread::spawn(move || {
            while let Some(frame) = alloy_ingot::read_frame(&mut stdout) {
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });

        Ok(Process {
            inner: Mutex::new(Inner {
                child: Some(child),
                stdin,
                frames: Some(rx),
                dead: None,
            }),
        })
    }

    /// Sends one request and waits for its reply. An `ok: false` reply
    /// is the ingot's own error; a transport failure kills the process.
    pub fn request(&self, body: &Value, timeout: Duration) -> Result<Value, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(why) = &inner.dead {
            return Err(why.clone());
        }

        let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let len = u32::try_from(bytes.len()).map_err(|_| "a request under 4GB".to_string())?;
        let sent = inner.stdin.as_mut().map(|s| {
            s.write_all(&len.to_le_bytes())
                .and_then(|()| s.write_all(&bytes))
                .and_then(|()| s.flush())
        });

        if let Some(Err(e)) = sent {
            let why = format!("the pipe to the ingot broke: {e}");
            inner.kill(why.clone());

            return Err(why);
        }

        let reply = match inner.frames.as_ref().map(|f| f.recv_timeout(timeout)) {
            Some(Ok(frame)) => frame,

            Some(Err(std::sync::mpsc::RecvTimeoutError::Timeout)) => {
                let why = format!("the ingot gave no answer in {} seconds", timeout.as_secs());
                inner.kill(why.clone());

                return Err(why);
            }

            _ => {
                let why = "the ingot closed its pipe".to_string();
                inner.kill(why.clone());

                return Err(why);
            }
        };
        let value: Value = serde_json::from_slice(&reply)
            .map_err(|e| format!("the ingot sent a reply that is not JSON: {e}"))?;

        if value["ok"].as_bool() == Some(true) {
            Ok(value)
        } else {
            Err(value["error"]
                .as_str()
                .unwrap_or("the ingot answered without ok or error")
                .to_string())
        }
    }

    /// Whether the process still answers.
    pub fn alive(&self) -> bool {
        self.inner.lock().map(|i| i.dead.is_none()).unwrap_or(false)
    }
}

impl Inner {
    fn kill(&mut self, why: String) {
        self.dead = Some(why);
        self.stdin = None;
        self.frames = None;

        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            // Closing stdin ends the ingot's serve loop; a wait reaps it.
            inner.stdin = None;

            if let Some(mut child) = inner.child.take() {
                // An ingot that ignores end of file gets one second, then
                // a kill; the host never waits on it.
                let deadline = std::time::Instant::now() + Duration::from_secs(1);

                while std::time::Instant::now() < deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        return;
                    }

                    std::thread::sleep(Duration::from_millis(20));
                }

                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}
