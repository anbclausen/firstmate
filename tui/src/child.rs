//! Runs the chosen harness as a real child process on a pty: raw output
//! bytes stream back to the TUI for the terminal emulator to interpret,
//! keystrokes go the other way down the same pty, and the pty follows the
//! pane's size so the harness lays itself out to fit.
//!
//! Output is forwarded verbatim rather than as lines, because a
//! full-screen harness addresses the cursor instead of printing lines.
//! `decision::Scanner` observes the same stream for the decision sentinel.

use std::io::{self, Read, Write};
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::decision::{Decision, Scanner};

pub enum ChildEvent {
    /// Raw pty output, to be fed to the terminal emulator unchanged.
    Output(Vec<u8>),
    Decision(Decision),
    DecisionParseError(String),
    Exited(i32),
}

/// A running harness: its output events, its input channel, and the
/// handles needed to resize and terminate it.
pub struct Child {
    pub events: Receiver<ChildEvent>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

impl Child {
    /// Sends already-encoded key bytes to the harness. See `keys::encode`.
    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Resizes the pty so the harness re-renders at the pane's dimensions.
    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Terminates the harness so it doesn't keep running detached after
    /// the TUI exits.
    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }
}

/// Spawns `command` on a pty sized to `rows` by `cols`.
///
/// The reader thread also owns waiting on the child, because it blocks in
/// `child.wait()` for the process's lifetime; the returned `Child` can
/// still kill it independently through its own killer handle.
pub fn spawn(
    command: &str,
    args: &[String],
    cwd: Option<&std::path::Path>,
    rows: u16,
    cols: u16,
) -> anyhow::Result<Child> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(command);
    cmd.args(args);
    // Without this the harness sees TERM=dumb and refuses to draw itself.
    cmd.env("TERM", "xterm-256color");
    // Without an explicit cwd, portable-pty starts the child in the user's
    // home (/home/agent in the runtime container), where firstmate's repo is
    // invisible. Pin it to the bind-mounted repo root so the harness opens
    // directly on the firstmate checkout.
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let writer = pair.master.take_writer()?;
    let mut reader = pair.master.try_clone_reader()?;
    let (tx, rx) = channel();

    thread::spawn(move || {
        let mut scanner = Scanner::new();
        let mut buf = [0u8; 8192];
        loop {
            let Ok(read) = reader.read(&mut buf) else { break };
            if read == 0 {
                break;
            }
            let chunk = &buf[..read];
            // Screen first, then any decision found on it, so the overlay
            // never appears over a pane that hasn't caught up yet.
            if tx.send(ChildEvent::Output(chunk.to_vec())).is_err() {
                return;
            }
            for found in scanner.push(chunk) {
                let event = match found {
                    Ok(decision) => ChildEvent::Decision(decision),
                    Err(err) => ChildEvent::DecisionParseError(err),
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
        }
        let status = child.wait().ok();
        let code = status.and_then(|s| s.exit_code().try_into().ok()).unwrap_or(-1);
        let _ = tx.send(ChildEvent::Exited(code));
    });

    Ok(Child {
        events: rx,
        writer,
        master: pair.master,
        killer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn drain_output(child: &Child, until: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while Instant::now() < deadline {
            let Ok(event) = child.events.recv_timeout(Duration::from_secs(5)) else {
                break;
            };
            match event {
                ChildEvent::Output(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                ChildEvent::Exited(_) => break,
                _ => {}
            }
            if until(&seen) {
                break;
            }
        }
        seen
    }

    /// The reader thread blocks in `child.wait()` for the process's full
    /// lifetime. If `kill()` didn't actually terminate the child, this test
    /// would hang until the 30s sleep finished (well past the recv timeout)
    /// instead of observing `Exited` almost immediately.
    #[test]
    fn kill_terminates_a_long_running_child_promptly() {
        let mut child = spawn("sleep", &["30".to_string()], None, 24, 80).unwrap();

        child.kill();

        let event = child.events.recv_timeout(Duration::from_secs(5));
        assert!(
            matches!(event, Ok(ChildEvent::Exited(_))),
            "expected the child to exit promptly after kill(), got {:?} instead",
            event.is_ok()
        );
    }

    /// The whole point of the rewrite: what the captain types has to reach
    /// the child. `cat` echoes it straight back down the pty.
    #[test]
    fn typed_input_reaches_the_child() {
        let mut child = spawn("cat", &[], None, 24, 80).unwrap();

        child.write_input(b"ahoy\r").unwrap();

        let seen = drain_output(&child, |s| s.contains("ahoy"));
        child.kill();
        assert!(seen.contains("ahoy"), "child never saw the input, got {seen:?}");
    }

    /// The pty's size is what the harness reads to lay itself out, so a
    /// resize has to reach the kernel's terminal, not just the emulator.
    #[test]
    fn resize_changes_the_size_the_child_sees() {
        let mut child = spawn("sh", &["-c".into(), "sleep 0.4; stty size".into()], None, 24, 80).unwrap();

        child.resize(31, 101).unwrap();

        let seen = drain_output(&child, |s| s.contains("31 101"));
        child.kill();
        assert!(seen.contains("31 101"), "expected the resized size, got {seen:?}");
    }
}
