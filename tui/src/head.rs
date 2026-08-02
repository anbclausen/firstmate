//! The captain-facing "figurehead": an animated ASCII first mate at the helm,
//! reflecting what the wrapped agent is doing, so the captain can read state at
//! a glance without watching the transcript.
//!
//! The art is one fixed frame with three animated slots - the eyes, the mouth,
//! and the helm the mate is steering - so a state is a set of slot frames
//! rather than a whole second drawing. The state itself is live: `main.rs`
//! sets it from what the session actually does, and `Head::settle` falls back
//! to idling after a quiet spell rather than leaving the last thing the
//! harness did on screen forever.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadState {
    Idle,
    Thinking,
    Talking,
    /// The harness is gone; the mate has left the helm.
    Gone,
}

/// The art around the animated slots: `EEEEE` is the eyes, `MMM` the mouth and
/// `H` the helm. Every state's frames are exactly their slot's width, so the
/// drawing keeps one shape and nothing shifts as it animates.
const FIGUREHEAD: [&str; 8] = [
    "        _-^-_        ",
    "     .-'     '-.     ",
    "     '-._____.-'     ",
    "      .-------.      ",
    "      | EEEEE |      ",
    "      |  MMM  |      ",
    "      '--___--'      ",
    "    ~~~~( H )~~~~    ",
];

/// Width of the drawing above, and the narrowest pane it fits in unclipped.
pub const FIGUREHEAD_WIDTH: u16 = 21;

impl HeadState {
    /// Eye frames, five cells wide.
    fn eyes(self) -> &'static [&'static str] {
        match self {
            HeadState::Idle => &["o   o", "o   o", "o   o", "-   -"],
            HeadState::Thinking => &["o   O", "O   o", "^   ^"],
            HeadState::Talking => &["o   o", "O   O"],
            HeadState::Gone => &["x   x"],
        }
    }

    /// Mouth frames, three cells wide.
    fn mouth(self) -> &'static [&'static str] {
        match self {
            HeadState::Idle => &["\\_/"],
            HeadState::Thinking => &["'''", " ~ "],
            HeadState::Talking => &[" o ", "\\_/", "ooo"],
            HeadState::Gone => &["___"],
        }
    }

    /// Helm frames, one cell wide: the wheel only turns while there is way on.
    fn helm(self) -> &'static [&'static str] {
        match self {
            HeadState::Idle => &["|"],
            HeadState::Thinking | HeadState::Talking => &["|", "/", "-", "\\"],
            HeadState::Gone => &["x"],
        }
    }

    fn color(self) -> Color {
        match self {
            HeadState::Idle => Color::Gray,
            HeadState::Thinking => Color::Yellow,
            HeadState::Talking => Color::Cyan,
            HeadState::Gone => Color::Red,
        }
    }

    fn label(self) -> &'static str {
        match self {
            HeadState::Idle => "idling",
            HeadState::Thinking => "thinking",
            HeadState::Talking => "talking",
            HeadState::Gone => "off watch",
        }
    }
}

/// One rendered frame of the figurehead for `state` at animation step `tick`.
/// Pure, so the state-to-art mapping is testable without a terminal.
fn figurehead_frame(state: HeadState, tick: usize) -> Vec<String> {
    let eyes = state.eyes()[tick % state.eyes().len()];
    let mouth = state.mouth()[tick % state.mouth().len()];
    let helm = state.helm()[tick % state.helm().len()];
    FIGUREHEAD
        .iter()
        .map(|line| line.replace("EEEEE", eyes).replace("MMM", mouth).replace('H', helm))
        .collect()
}

pub struct Head {
    state: HeadState,
    tick: usize,
    /// When the current state was last confirmed by something the session
    /// actually did; `settle` reads it to fall back to idling.
    since: Instant,
}

impl Head {
    pub fn new() -> Self {
        Head {
            state: HeadState::Idle,
            tick: 0,
            since: Instant::now(),
        }
    }

    pub fn set_state(&mut self, state: HeadState) {
        self.since = Instant::now();
        if state != self.state {
            self.state = state;
            self.tick = 0;
        }
    }

    /// Fall back to idling once the session has been quiet for `quiet`.
    /// Talking and thinking are both live states: neither is true any more
    /// once the harness has stopped producing anything. A pending decision is
    /// the exception - it produces no further output, but the session really is
    /// blocked on it, so it holds the state until the captain answers.
    /// Returns whether the state changed, so a settled fleet only redraws once.
    pub fn settle(&mut self, quiet: Duration, decision_pending: bool) -> bool {
        let live = matches!(self.state, HeadState::Talking | HeadState::Thinking);
        if !live || decision_pending || self.since.elapsed() < quiet {
            return false;
        }
        self.set_state(HeadState::Idle);
        true
    }

    /// Advance the animation by one frame; call this on a fixed timer.
    pub fn advance(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let color = self.state.color();
        let mut lines: Vec<Line> = figurehead_frame(self.state, self.tick)
            .into_iter()
            .map(|art| Line::from(Span::styled(art, Style::default().fg(color))))
            .collect();
        lines.push(Line::from(Span::styled(
            self.state.label(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));

        let block = Block::default()
            .borders(Borders::ALL)
            .title("firstmate")
            .style(Style::default().fg(color));
        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(paragraph, area);
    }
}

impl Default for Head {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [HeadState; 4] = [
        HeadState::Idle,
        HeadState::Thinking,
        HeadState::Talking,
        HeadState::Gone,
    ];

    #[test]
    fn switching_state_resets_the_animation_tick() {
        let mut head = Head::new();
        head.advance();
        head.advance();
        assert_eq!(head.tick, 2);
        head.set_state(HeadState::Thinking);
        assert_eq!(head.tick, 0);
    }

    #[test]
    fn setting_the_same_state_does_not_reset_the_tick() {
        let mut head = Head::new();
        head.advance();
        head.set_state(HeadState::Idle);
        assert_eq!(head.tick, 1);
    }

    /// The slots have to be exactly the width of their placeholders, or the
    /// drawing shifts around as it animates.
    #[test]
    fn every_state_has_frames_that_fit_their_slots() {
        for state in ALL {
            assert!(!state.eyes().is_empty());
            assert!(state.eyes().iter().all(|f| f.chars().count() == 5));
            assert!(state.mouth().iter().all(|f| f.chars().count() == 3));
            assert!(state.helm().iter().all(|f| f.chars().count() == 1));
        }
    }

    /// Whatever the state and however far the animation has run, the drawing
    /// keeps one shape and one width, so it never clips or jitters.
    #[test]
    fn every_frame_keeps_the_same_shape() {
        for state in ALL {
            for tick in 0..12 {
                let art = figurehead_frame(state, tick);
                assert_eq!(art.len(), FIGUREHEAD.len());
                assert!(art
                    .iter()
                    .all(|line| line.chars().count() == usize::from(FIGUREHEAD_WIDTH)));
                assert!(
                    !art.concat().contains('E') && !art.concat().contains('M'),
                    "a slot was left unfilled in {art:?}"
                );
            }
        }
    }

    /// The figurehead has to show the live state, so a state that is only true
    /// while the harness is producing must not outlive it.
    #[test]
    fn a_quiet_session_settles_back_to_idling() {
        let mut head = Head::new();
        head.set_state(HeadState::Talking);
        assert!(!head.settle(Duration::from_secs(60), false), "still live");
        assert_eq!(head.state, HeadState::Talking);

        assert!(head.settle(Duration::ZERO, false));
        assert_eq!(head.state, HeadState::Idle);
        assert!(!head.settle(Duration::ZERO, false), "idling is already settled");
    }

    /// A decision box is a live blocked state, so the quiet timer must not
    /// flip the figurehead to idling while the captain is being asked.
    #[test]
    fn a_pending_decision_holds_the_thinking_state() {
        let mut head = Head::new();
        head.set_state(HeadState::Thinking);
        assert!(!head.settle(Duration::ZERO, true), "a decision is still up");
        assert_eq!(head.state, HeadState::Thinking);

        assert!(head.settle(Duration::ZERO, false), "dismissed, so it settles");
        assert_eq!(head.state, HeadState::Idle);
    }

    /// A dead harness is a fact, not a lull, so it must survive the quiet timer.
    #[test]
    fn a_gone_harness_does_not_settle_to_idling() {
        let mut head = Head::new();
        head.set_state(HeadState::Gone);
        assert!(!head.settle(Duration::ZERO, false));
        assert_eq!(head.state, HeadState::Gone);
    }
}
