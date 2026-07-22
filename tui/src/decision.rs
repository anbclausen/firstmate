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
}
