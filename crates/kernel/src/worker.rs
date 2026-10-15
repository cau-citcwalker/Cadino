//! Kernel operations in a worker process, so that an OpenCASCADE crash (an uncaught C++ exception
//! aborts) fails one operation instead of ending the app.
//!
//! The worker is the app's own executable started with [`crate::WORKER_ENV`] set. It reads one
//! [`Request`] as JSON from standard input and writes one reply line to standard output, marked
//! so that whatever else OpenCASCADE prints there is skipped.
//!
//! ponytail: one process per operation (about 0.1 s each). Upgrade path: a long-lived
//! worker, restarted after a crash, if opening files with many solids gets slow.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

use crate::{KernelError, Reply, Request, Result};

/// Starts the reply line.
const MARK: &str = "@@cadrhino-kernel-reply@@";
/// Longest an operation may run before the worker is stopped.
const TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct Worker {
    exe: PathBuf,
    args: Vec<String>,
}

static WORKER: OnceLock<Worker> = OnceLock::new();

pub(crate) fn register(exe: PathBuf, args: Vec<String>) {
    let _ = WORKER.set(Worker { exe, args });
}

pub(crate) fn registered() -> Option<&'static Worker> {
    WORKER.get()
}

/// The worker's side: runs the request on standard input, writes the reply and exits.
pub(crate) fn serve() -> ! {
    let mut input = Vec::new();
    let reply = match std::io::stdin().read_to_end(&mut input) {
        Ok(_) => serde_json::from_slice::<Request>(&input).map_err(|e| KernelError::Failed(format!("bad request: {e}"))).and_then(|r| r.serve()),
        Err(e) => Err(KernelError::Failed(format!("reading the request: {e}"))),
    };
    let line = serde_json::to_string(&reply).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\n{MARK}{line}\n").and_then(|_| out.flush());
    std::process::exit(0)
}

impl Worker {
    /// Runs the request in a new worker process.
    pub(crate) fn call(&self, req: &Request) -> Result<Reply> {
        let what = req.what();
        let failed = |e: std::io::Error| KernelError::Failed(format!("could not start the worker process while {what}: {e}"));
        let body = serde_json::to_vec(req).map_err(|e| KernelError::Failed(format!("{what}: {e}")))?;
        let mut cmd = Command::new(&self.exe);
        cmd.args(&self.args).env(crate::WORKER_ENV, "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window flashes up.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(failed)?;
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        let (tx, rx) = mpsc::channel();
        // Writing and reading on their own threads: neither side can block the other, and the
        // timeout holds whatever the worker does.
        let threads = std::thread::Builder::new()
            .spawn(move || {
                if let Some(mut i) = stdin {
                    let _ = i.write_all(&body);
                }
            })
            .and_then(|_| {
                std::thread::Builder::new().spawn(move || {
                    let mut out = Vec::new();
                    if let Some(mut o) = stdout {
                        let _ = o.read_to_end(&mut out);
                    }
                    let _ = tx.send(out);
                })
            });
        let late = || KernelError::Failed(format!("it took longer than {} s while {what} and was stopped", TIMEOUT.as_secs()));
        let out = match threads.map_err(failed).and_then(|_| rx.recv_timeout(TIMEOUT).map_err(|_| late())) {
            Ok(out) => out,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        let status = child.wait().map(|s| format!(" ({s})")).unwrap_or_default();
        reply(&out).unwrap_or_else(|| Err(KernelError::Failed(format!("it stopped (crashed) while {what}{status}"))))
    }
}

/// The reply line in the worker's output; `None` when it has none (it crashed).
fn reply(out: &[u8]) -> Option<Result<Reply>> {
    let text = String::from_utf8_lossy(out);
    let at = text.find(&format!("\n{MARK}"))? + 1 + MARK.len();
    serde_json::from_str(text.get(at..)?.lines().next()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_is_found_among_other_output() {
        let line = serde_json::to_string(&Ok::<_, KernelError>(Reply::Done)).unwrap();
        let out = format!("Not a location table\nrunning 1 test\n{MARK}{line}\nmore noise\n");
        assert!(matches!(reply(out.as_bytes()), Some(Ok(Reply::Done))));
        let out = format!("noise\n{MARK}{}\n", serde_json::to_string(&Err::<Reply, _>(KernelError::Invalid("x".into()))).unwrap());
        assert_eq!(reply(out.as_bytes()).map(|r| r.err()), Some(Some(KernelError::Invalid("x".into()))));
        assert!(reply(b"crashed before replying").is_none());
        assert!(reply(format!("\n{MARK}{{\"Ok\":").as_bytes()).is_none());
    }
}
