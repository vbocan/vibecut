//! Cooperative cancellation: one tap stops a wrong render.
//!
//! A [`CancelToken`] is installed together with the progress reporter
//! ([`crate::progress::with_sink_and_cancel`]) and travels wherever the
//! reporter goes — work fanned out to other threads enters a reporter
//! cloned from [`crate::progress::current`]. Once the token is cancelled:
//!
//! - every ffmpeg / ffprobe run through [`CancellableCommand`] or
//!   [`wait_child`] kills its child process (polling `try_wait` with a
//!   short back-off) and returns [`Cancelled`];
//! - explicit [`check`] points in the pipeline stop it where an ffmpeg
//!   failure would otherwise be swallowed, and before the outputs are
//!   committed (see [`crate::staging`]);
//! - progress reports stop reaching the sink, so the bar freezes.
//!
//! A cancelled render returns an error whose chain contains
//! [`Cancelled`]; callers tell it from a real failure with
//! [`is_cancelled`]. Without a token installed (the CLI) every helper
//! here behaves exactly like its `std::process` counterpart.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Shared stop flag. Cloning shares the flag; cancelling is permanent.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask every holder of this token to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The error a render stops with once its token is cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("render cancelled")]
pub struct Cancelled;

/// True when `e` (or anything it wraps) is a [`Cancelled`].
pub fn is_cancelled(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Cancelled>())
}

/// `Err(Cancelled)` once this thread's render has been cancelled.
pub fn check() -> Result<(), Cancelled> {
    match crate::progress::cancel_token() {
        Some(t) if t.is_cancelled() => Err(Cancelled),
        _ => Ok(()),
    }
}

/// Longest nap between two `try_wait` polls. The first polls are 1 ms
/// apart so a 30 ms ffprobe isn't slowed down; long encodes settle at
/// this, which bounds how long a kill can lag behind a cancel.
const POLL_MAX: Duration = Duration::from_millis(25);

/// Wait for `child` to exit, killing it if this thread's render is
/// cancelled first. Without a token installed this is `child.wait()`.
pub fn wait_child(child: &mut Child) -> anyhow::Result<ExitStatus> {
    match crate::progress::cancel_token() {
        Some(token) => wait_child_with(child, &token),
        None => Ok(child.wait()?),
    }
}

/// [`wait_child`] against an explicit token. On cancel the child is
/// killed and reaped (no zombie) before `Cancelled` is returned.
pub fn wait_child_with(child: &mut Child, token: &CancelToken) -> anyhow::Result<ExitStatus> {
    let mut nap = Duration::from_millis(1);
    loop {
        if token.is_cancelled() {
            // Err here only means it already exited; reap it either way.
            let _ = child.kill();
            let _ = child.wait();
            return Err(Cancelled.into());
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(POLL_MAX);
    }
}

/// Cancellable versions of [`Command::output`] and [`Command::status`]:
/// same pipes, same results, but the child is killed when the render is
/// cancelled. Swap `.output()` for `.cancellable_output()` at a call site.
pub trait CancellableCommand {
    fn cancellable_output(&mut self) -> anyhow::Result<Output>;
    fn cancellable_status(&mut self) -> anyhow::Result<ExitStatus>;
}

impl CancellableCommand for Command {
    fn cancellable_output(&mut self) -> anyhow::Result<Output> {
        let Some(token) = crate::progress::cancel_token() else {
            return Ok(self.output()?);
        };
        if token.is_cancelled() {
            return Err(Cancelled.into());
        }
        // What `output()` does: stdin closed, stdout + stderr captured.
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // Drain both pipes while waiting, or a chatty child blocks on a
        // full pipe and never exits. A kill closes them, so the drains end.
        let out = child.stdout.take().map(drain);
        let err = child.stderr.take().map(drain);
        let status = wait_child_with(&mut child, &token);
        let stdout = out.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        Ok(Output { status: status?, stdout, stderr })
    }

    fn cancellable_status(&mut self) -> anyhow::Result<ExitStatus> {
        let Some(token) = crate::progress::cancel_token() else {
            return Ok(self.status()?);
        };
        if token.is_cancelled() {
            return Err(Cancelled.into());
        }
        let mut child = self.spawn()?;
        wait_child_with(&mut child, &token)
    }
}

fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// A pid is alive while /proc lists it (Linux; the Docker image and
    /// CI). Elsewhere the test relies on `wait` having reaped the child.
    fn pid_alive(pid: u32) -> bool {
        let p = std::path::Path::new("/proc").join(pid.to_string());
        if !std::path::Path::new("/proc/self").exists() {
            return false;
        }
        // A zombie still has a /proc entry; count it as alive — reaping
        // is part of the contract.
        p.exists()
    }

    #[test]
    fn token_is_shared_and_permanent() {
        let t = CancelToken::new();
        let u = t.clone();
        assert!(!u.is_cancelled());
        t.cancel();
        assert!(u.is_cancelled());
        t.cancel();
        assert!(u.is_cancelled());
    }

    #[test]
    fn cancelled_is_found_through_context() {
        use anyhow::Context;
        let e: anyhow::Error = Err::<(), _>(Cancelled)
            .context("normalize segment 3")
            .context("render")
            .unwrap_err();
        assert!(is_cancelled(&e));
        assert!(!is_cancelled(&anyhow::anyhow!("ffmpeg failed (exit 1)")));
    }

    #[test]
    fn without_a_token_check_passes_and_commands_run_normally() {
        assert!(check().is_ok());
        if !ffmpeg_available() {
            return;
        }
        let out = Command::new("ffmpeg").arg("-version").cancellable_output().unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("ffmpeg version"));
    }

    #[test]
    fn output_is_captured_under_a_token() {
        if !ffmpeg_available() {
            return;
        }
        let token = CancelToken::new();
        let out = crate::progress::with_sink_and_cancel(|_| {}, token, || {
            Command::new("ffmpeg").arg("-version").cancellable_output()
        })
        .unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("ffmpeg version"));
    }

    #[test]
    fn an_already_cancelled_render_spawns_nothing() {
        let token = CancelToken::new();
        token.cancel();
        let r = crate::progress::with_sink_and_cancel(|_| {}, token, || {
            // Would fail to spawn if it got that far.
            Command::new("definitely-not-a-binary-vibecut").cancellable_output()
        });
        assert!(is_cancelled(&r.unwrap_err()));
    }

    /// The contract the Stop button relies on: a long ffmpeg encode is
    /// killed (and reaped) promptly once the render is cancelled.
    #[test]
    fn cancelling_kills_a_running_ffmpeg() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let token = CancelToken::new();
        let canceller = token.clone();
        // A 10-minute lavfi encode: it can only end early if it's killed.
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-nostdin",
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
                "-t", "600",
                "-c:v", "libx264", "-preset", "ultrafast",
                "-f", "null", "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ffmpeg");
        let pid = child.id();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            canceller.cancel();
            Instant::now()
        });
        let started = Instant::now();
        let r = crate::progress::with_sink_and_cancel(|_| {}, token, || wait_child(&mut child));
        let cancelled_at = stopper.join().unwrap();
        let err = r.expect_err("a cancelled wait must not report the exit status");
        assert!(is_cancelled(&err));
        // Stopped promptly after the cancel, long before the encode's end.
        assert!(cancelled_at.elapsed() < Duration::from_millis(500), "kill lagged the cancel");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!pid_alive(pid), "ffmpeg {pid} still running (or unreaped) after cancel");
    }

    /// Same through the `output()` replacement the pipeline uses.
    #[test]
    fn cancellable_output_kills_its_child() {
        if !ffmpeg_available() {
            return;
        }
        let token = CancelToken::new();
        let canceller = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            canceller.cancel();
        });
        let started = Instant::now();
        let r = crate::progress::with_sink_and_cancel(|_| {}, token, || {
            Command::new("ffmpeg")
                .args([
                    "-v", "error", "-nostdin",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "600",
                    "-f", "s16le", "-",
                ])
                .cancellable_output()
        });
        assert!(is_cancelled(&r.unwrap_err()));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
