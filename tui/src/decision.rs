//! The decision protocol: the one wire format a wrapped agent uses to tell
//! the TUI "stop and show the captain a decision box" instead of scrolling
//! past it in dimmed output.
//!
//! Wire format: a single line of JSON on its own line in the agent's stdout,
//! wrapped in a sentinel so it can be told apart from ordinary chatter:
//!
//! ```text
//! ::firstmate-decision:: {"prompt": "...", "options": ["...", "..."]}
//! ```
//!
//! `prompt` is the question shown in the decision box.
//! `options` is the agent's own list of choices, in display order.
//! The TUI always appends two more choices after the agent's own list,
//! never supplied by the agent: "Something else" and "Chat about this".
//! Selecting either does not resolve the decision by itself; it hands
//! control back to a free-text reply channel instead of a fixed choice.
//! This file is the only owner of this contract; anything else that needs
//! to describe it should link here rather than restate the schema.

use serde::Deserialize;

pub const SENTINEL: &str = "::firstmate-decision::";

pub const ALWAYS_AVAILABLE: [&str; 2] = ["Something else", "Chat about this"];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RawDecision {
    pub prompt: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub prompt: String,
    /// The agent's own options, followed by the two always-available choices.
    pub options: Vec<String>,
}

impl From<RawDecision> for Decision {
    fn from(raw: RawDecision) -> Self {
        let mut options = raw.options;
        options.extend(ALWAYS_AVAILABLE.iter().map(|s| s.to_string()));
        Decision {
            prompt: raw.prompt,
            options,
        }
    }
}

/// Scan a line of agent output for the decision sentinel and parse it.
/// Returns `None` for an ordinary output line, `Some(Err(_))` for a line
/// that carries the sentinel but fails to parse, so callers can surface a
/// malformed-decision error instead of silently dropping it.
pub fn parse_line(line: &str) -> Option<Result<Decision, serde_json::Error>> {
    let rest = line.trim_start().strip_prefix(SENTINEL)?;
    Some(serde_json::from_str::<RawDecision>(rest.trim()).map(Decision::from))
}

/// A line accumulator over the raw pty byte stream.
///
/// The agent pane is a real terminal now, so the same bytes go to the
/// emulator; this only observes them on the way past, reassembling lines
/// across read boundaries so a sentinel split over two chunks is still
/// found.
pub struct Scanner {
    line: Vec<u8>,
}

/// A line that overruns this without a newline cannot be a decision line
/// and is almost certainly cursor-addressed screen drawing, so the partial
/// line is dropped rather than buffered without bound.
const MAX_LINE: usize = 64 * 1024;

impl Scanner {
    pub fn new() -> Self {
        Scanner { line: Vec::new() }
    }

    /// Feeds one chunk of pty output and returns every decision found on a
    /// line completed by it, in order. `Err` carries a malformed payload's
    /// parse error so callers can surface it instead of dropping it.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Decision, String>> {
        let mut found = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                let line = String::from_utf8_lossy(&self.line);
                if let Some(result) = parse_line(line.trim_end_matches('\r')) {
                    found.push(result.map_err(|err| err.to_string()));
                }
                self.line.clear();
            } else {
                if self.line.len() >= MAX_LINE {
                    self.line.clear();
                }
                self.line.push(byte);
            }
        }
        found
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_output_is_not_a_decision() {
        assert!(parse_line("just some agent chatter").is_none());
    }

    #[test]
    fn parses_a_well_formed_decision_and_appends_fallbacks() {
        let line = r#"::firstmate-decision:: {"prompt": "merge now?", "options": ["yes", "no"]}"#;
        let decision = parse_line(line).unwrap().unwrap();
        assert_eq!(decision.prompt, "merge now?");
        assert_eq!(
            decision.options,
            vec!["yes", "no", "Something else", "Chat about this"]
        );
    }

    #[test]
    fn tolerates_leading_whitespace_before_the_sentinel() {
        let line = format!("   {SENTINEL} {{\"prompt\": \"p\", \"options\": []}}");
        let decision = parse_line(&line).unwrap().unwrap();
        assert_eq!(decision.prompt, "p");
        assert_eq!(decision.options, vec!["Something else", "Chat about this"]);
    }

    #[test]
    fn malformed_payload_after_sentinel_is_a_reported_error_not_a_silent_drop() {
        let line = format!("{SENTINEL} not json");
        assert!(parse_line(&line).unwrap().is_err());
    }

    #[test]
    fn empty_options_list_still_yields_both_always_available_choices() {
        let line = format!("{SENTINEL} {{\"prompt\": \"p\", \"options\": []}}");
        let decision = parse_line(&line).unwrap().unwrap();
        assert_eq!(decision.options, ALWAYS_AVAILABLE.to_vec());
    }

    const DECISION_LINE: &str =
        r#"::firstmate-decision:: {"prompt": "merge now?", "options": ["yes"]}"#;

    #[test]
    fn scanner_finds_a_decision_on_a_completed_line() {
        let mut scanner = Scanner::new();
        assert!(scanner.push(b"ordinary output\r\n").is_empty());
        let found = scanner.push(format!("{DECISION_LINE}\r\n").as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].as_ref().unwrap().prompt, "merge now?");
    }

    /// A pty read can split anywhere, including mid-sentinel, so the
    /// scanner has to reassemble across chunk boundaries.
    #[test]
    fn scanner_reassembles_a_decision_split_across_chunks() {
        let mut scanner = Scanner::new();
        let (head, tail) = DECISION_LINE.split_at(10);
        assert!(scanner.push(head.as_bytes()).is_empty());
        let found = scanner.push(format!("{tail}\n").as_bytes());
        assert_eq!(found.len(), 1);
        assert!(found[0].is_ok());
    }

    /// A decision is only reported once its line is terminated, so a
    /// half-written payload never parses as a malformed one.
    #[test]
    fn scanner_reports_nothing_until_the_line_is_terminated() {
        let mut scanner = Scanner::new();
        assert!(scanner.push(DECISION_LINE.as_bytes()).is_empty());
        assert_eq!(scanner.push(b"\n").len(), 1);
    }

    #[test]
    fn scanner_surfaces_a_malformed_payload_as_an_error() {
        let mut scanner = Scanner::new();
        let found = scanner.push(format!("{SENTINEL} not json\n").as_bytes());
        assert_eq!(found.len(), 1);
        assert!(found[0].is_err());
    }

    /// Escape-heavy screen drawing from a full-screen harness must pass
    /// through without being mistaken for a decision.
    #[test]
    fn scanner_ignores_terminal_control_sequences() {
        let mut scanner = Scanner::new();
        assert!(scanner.push(b"\x1b[2J\x1b[H\x1b[31mhello\x1b[0m\r\n").is_empty());
    }

    /// An unterminated line must not grow without bound on a stream that
    /// never sends a newline.
    #[test]
    fn scanner_drops_an_unterminated_line_past_the_cap() {
        let mut scanner = Scanner::new();
        scanner.push(&vec![b'x'; MAX_LINE * 2]);
        assert!(scanner.line.len() <= MAX_LINE);
    }
}
