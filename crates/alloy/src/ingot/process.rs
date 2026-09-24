//! One running ingot: the child process and the framed pipe to it.
//!
//! Requests go one at a time. A reader thread turns the child's stdout
//! into frames on a channel, so a request can wait with a timeout: a
//! hung ingot costs one request, never the editor. After a timeout or a
//! broken pipe the process is dead, and the next request starts it
//! again with the same init. An ingot that dies on every request, or
//! hangs three times, stays dead, and every later request answers with
//! its error until a reload.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::Value;

/// How long a request may take before the ingot counts as hung.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// How many restarts in a row may fail before the ingot stays dead. A
/// request that gets an answer resets the count.
const RESTARTS: u32 = 3;

/// How many timeouts the process may cost in all. Each one is a full
/// wait, so an ingot that hangs on every file stops being asked.
const HANGS: u32 = 3;

pub struct Process {
    inner: Mutex<Inner>,
}

/// One frame from the reader thread, or why the pipe can carry no more.
type Frame = Result<Vec<u8>, String>;

struct Inner {
    binary: PathBuf,
    dir: PathBuf,
    /// The init request, sent again after a restart.
    init: Option<Value>,
    child: Option<Child>,
    stdin: Option<std::process::ChildStdin>,
    frames: Option<Receiver<Frame>>,
    /// Why the process is gone, once it is.
    dead: Option<String>,
    /// The restarts since the last answer.
    restarts: u32,
    /// The timeouts so far.
    hangs: u32,
}

impl Process {
    /// Starts the binary with the working directory of its manifest.
    pub fn start(binary: &Path, dir: &Path) -> Result<Process, String> {
        let mut inner = Inner {
            binary: binary.to_path_buf(),
            dir: dir.to_path_buf(),
            init: None,
            child: None,
            stdin: None,
            frames: None,
            dead: None,
            restarts: 0,
            hangs: 0,
        };
        inner.spawn()?;

        Ok(Process {
            inner: Mutex::new(inner),
        })
    }

    /// Sends the init request, and keeps it for a restart.
    pub fn init(&self, body: Value) -> Result<Value, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let reply = inner.exchange(&body, TIMEOUT);
        inner.init = Some(body);

        reply
    }

    /// Sends one request and waits for its reply. An `ok: false` reply
    /// is the ingot's own error; a transport failure kills the process,
    /// and the next request starts it again.
    pub fn request(&self, body: &Value, timeout: Duration) -> Result<Value, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(why) = inner.dead.clone() {
            if inner.restarts >= RESTARTS || inner.hangs >= HANGS {
                return Err(why);
            }

            inner
                .restart()
                .map_err(|e| format!("{why}; the restart failed: {e}"))?;
        }

        let reply = inner.exchange(body, timeout);

        // An answer, even an error, shows the ingot works. An init that
        // passes does not, since the ingot may die on every file.
        if inner.dead.is_none() {
            inner.restarts = 0;
        }

        reply
    }

    /// Whether the process still answers.
    pub fn alive(&self) -> bool {
        self.inner.lock().map(|i| i.dead.is_none()).unwrap_or(false)
    }
}

impl Inner {
    fn spawn(&mut self) -> Result<(), String> {
        let mut child = Command::new(&self.binary)
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.binary.display()))?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = channel();

        std::thread::spawn(move || {
            loop {
                let mut head = [0u8; 4];

                if stdout.read_exact(&mut head).is_err() {
                    return;
                }

                let len = u32::from_le_bytes(head) as usize;

                // Text reads as a huge length, and a wait for that body
                // lasts until the timeout.
                let frame = match len > alloy_ingot::MAX_FRAME {
                    true => Err(format!(
                        "the ingot wrote text on stdout, which carries the frames ({:?}); \
                         an ingot logs to stderr",
                        String::from_utf8_lossy(&head)
                    )),

                    false => {
                        let mut body = vec![0u8; len];

                        match stdout.read_exact(&mut body) {
                            Ok(()) => Ok(body),

                            Err(_) => return,
                        }
                    }
                };
                let stop = frame.is_err();

                if tx.send(frame).is_err() || stop {
                    return;
                }
            }
        });

        self.child = Some(child);
        self.stdin = stdin;
        self.frames = Some(rx);
        self.dead = None;

        Ok(())
    }

    fn restart(&mut self) -> Result<(), String> {
        self.restarts += 1;
        self.spawn()?;

        if let Some(init) = self.init.clone()
            && let Err(why) = self.exchange(&init, TIMEOUT)
        {
            return Err(self.kill(why));
        }

        Ok(())
    }

    /// One request and its reply over the live pipe.
    fn exchange(&mut self, body: &Value, timeout: Duration) -> Result<Value, String> {
        let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let len = u32::try_from(bytes.len()).map_err(|_| "a request under 4GB".to_string())?;
        let sent = self.stdin.as_mut().map(|s| {
            s.write_all(&len.to_le_bytes())
                .and_then(|()| s.write_all(&bytes))
                .and_then(|()| s.flush())
        });

        if let Some(Err(e)) = sent {
            let why = format!("the pipe to the ingot broke: {e}");

            return Err(self.kill(why));
        }

        let reply = match self.frames.as_ref().map(|f| f.recv_timeout(timeout)) {
            Some(Ok(Ok(frame))) => frame,

            Some(Ok(Err(why))) => return Err(self.kill(why)),

            Some(Err(std::sync::mpsc::RecvTimeoutError::Timeout)) => {
                let why = format!("the ingot gave no answer in {} seconds", timeout.as_secs());
                self.hangs += 1;

                return Err(self.kill(why));
            }

            _ => {
                let why = match self.exit_status() {
                    Some(status) => format!("the ingot closed its pipe and exited ({status})"),

                    None => "the ingot closed its pipe".to_string(),
                };

                return Err(self.kill(why));
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

    /// The exit status of a child that closed its pipe. It may close
    /// stdout a moment before it exits, so this waits up to a second.
    fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        let child = self.child.as_mut()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(1);

        while std::time::Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                return Some(status);
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        None
    }

    /// Stops the child and records why; gives the reason back.
    fn kill(&mut self, why: String) -> String {
        self.dead = Some(why.clone());
        self.stdin = None;
        self.frames = None;

        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        why
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
