//! The notification ping: one short sound when the session hands the keyboard
//! back to the captain, so a captain looking elsewhere knows their turn came.
//!
//! It is the terminal's own bell rather than an audio library, for the same
//! reason herdr's ping is: the TUI's real process runs inside a container with
//! no audio device, while the bell travels out over the pty to whatever
//! terminal the captain is actually sitting at, which plays the notification
//! sound they configured there. That also makes it free to fail - a terminal
//! with the bell off, a redirected stdout, or a headless CI run simply makes no
//! sound, and nothing here blocks or errors.

use std::io::{self, Write};

/// Sound one ping. Never blocks and never fails: a write error means the
/// captain hears nothing, which is the correct degraded behaviour.
pub fn ping() {
    let mut out = io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}
