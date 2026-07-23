//! Runs the chosen harness as a real child process on a pty and streams its
//! output back to the TUI a line at a time, so the transcript region can
//! render it dimmed and the decision parser can scan it for the sentinel.

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, PtySize};

use crate::decision::{parse_line, Decision};

pub enum ChildEvent {
    Line(String),
    Decision(Decision),
    DecisionParseError(String),
    Exited(i32),
}

/// Spawns `command` on a pty and returns a receiver of its output events
/// plus a killer handle that can terminate the child independently of the
/// reader thread, which otherwise blocks in `child.wait()` for the
/// lifetime of the process.
pub fn spawn(
    command: &str,
    args: &[String],
) -> anyhow::Result<(Receiver<ChildEvent>, Box<dyn ChildKiller + Send + Sync>)> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(command);
    cmd.args(args);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let killer = child.clone_killer();

    let reader = pair.master.try_clone_reader()?;
    let (tx, rx) = channel();

    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(line) = line else { break };
            match parse_line(&line) {
                None => {
                    let _ = tx.send(ChildEvent::Line(line));
                }
                Some(Ok(decision)) => {
                    let _ = tx.send(ChildEvent::Decision(decision));
                }
                Some(Err(err)) => {
                    let _ = tx.send(ChildEvent::DecisionParseError(err.to_string()));
                }
            }
        }
        let status = child.wait().ok();
        let code = status.and_then(|s| s.exit_code().try_into().ok()).unwrap_or(-1);
        let _ = tx.send(ChildEvent::Exited(code));
    });

    Ok((rx, killer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The reader thread blocks in `child.wait()` for the process's full
    /// lifetime. If `kill()` didn't actually terminate the child, this test
    /// would hang until the 30s sleep finished (well past the recv timeout)
    /// instead of observing `Exited` almost immediately.
    #[test]
    fn kill_terminates_a_long_running_child_promptly() {
        let (rx, mut killer) = spawn("sleep", &["30".to_string()]).unwrap();

        killer.kill().unwrap();

        let event = rx.recv_timeout(Duration::from_secs(5));
        assert!(
            matches!(event, Ok(ChildEvent::Exited(_))),
            "expected the child to exit promptly after kill(), got {:?} instead",
            event.is_ok()
        );
    }
}
