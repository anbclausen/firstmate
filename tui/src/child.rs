//! Runs the chosen harness as a real child process on a pty and streams its
//! output back to the TUI a line at a time, so the transcript region can
//! render it dimmed and the decision parser can scan it for the sentinel.

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::decision::{parse_line, Decision};

pub enum ChildEvent {
    Line(String),
    Decision(Decision),
    DecisionParseError(String),
    Exited(i32),
}

/// Spawns `command` on a pty and returns a receiver of its output events.
/// The reader thread runs for the lifetime of the child; the channel closes
/// when the child exits and its output is fully drained.
pub fn spawn(command: &str, args: &[String]) -> anyhow::Result<Receiver<ChildEvent>> {
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

    Ok(rx)
}
