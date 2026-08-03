//! The captain-facing "figurehead": an ASCII ship whose posture reflects what
//! the wrapped agent is doing, so the captain can read state at a glance
//! without watching the transcript.
//!
//! The ship has two drawings - sailing when there is way on, anchored when
//! there is not - each the same block of lines, with animated cells for the
//! masthead pennant and the water so a live ship moves and a resting one does
//! not. The state itself is live: `main.rs` sets it from what the session
//! actually does, and `Head::settle` falls back to idling after a quiet spell
//! rather than leaving the last thing the harness did on screen forever.

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

/// Which drawing a state gets: under way, or at rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ship {
    Sailing,
    Anchored,
}

/// Full sail and a wake. `P` is the masthead pennant and `W` the water; both
/// are single cells replaced in place, so the drawing keeps one shape.
const SAILING: [&str; 8] = [
    "        |P           ",
    "        |=\\          ",
    "        |==\\         ",
    "        |===\\        ",
    "        |====\\       ",
    "        |_____\\      ",
    "     \\_________/     ",
    "WWWWWWWWWWWWWWWWWWWWW",
];

/// Sail furled on the yard, anchor down on its line.
const ANCHORED: [&str; 8] = [
    "        |            ",
    "     ___|___         ",
    "    (_______)        ",
    "        |            ",
    "        |            ",
    "     \\_________/     ",
    "WWWWWWWWWWWWWWW|WWWWW",
    "              \\_/    ",
];

/// Width of the drawings above, and the narrowest pane they fit in unclipped.
pub const FIGUREHEAD_WIDTH: u16 = 21;

impl HeadState {
    /// A session that is producing is under way; anything else is at rest.
    fn ship(self) -> Ship {
        match self {
            HeadState::Thinking | HeadState::Talking => Ship::Sailing,
            HeadState::Idle | HeadState::Gone => Ship::Anchored,
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

/// One water cell: a wake running under a sailing ship, still water under an
/// anchored one.
fn water(ship: Ship, col: usize, tick: usize) -> char {
    match ship {
        Ship::Sailing => {
            if (col + tick) % 4 == 0 {
                '-'
            } else {
                '~'
            }
        }
        Ship::Anchored => '~',
    }
}

/// One rendered frame of the figurehead for `state` at animation step `tick`.
/// Pure, so the state-to-art mapping is testable without a terminal.
fn figurehead_frame(state: HeadState, tick: usize) -> Vec<String> {
    let ship = state.ship();
    let art = match ship {
        Ship::Sailing => &SAILING,
        Ship::Anchored => &ANCHORED,
    };
    let pennant = ['>', '>', '~', '-'][tick % 4];
    art.iter()
        .map(|line| {
            line.chars()
                .enumerate()
                .map(|(col, cell)| match cell {
                    'W' => water(ship, col, tick),
                    'P' => pennant,
                    other => other,
                })
                .collect()
        })
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

    /// A producing session is under way; anything else is at rest.
    #[test]
    fn only_a_live_session_is_under_sail() {
        assert_eq!(HeadState::Thinking.ship(), Ship::Sailing);
        assert_eq!(HeadState::Talking.ship(), Ship::Sailing);
        assert_eq!(HeadState::Idle.ship(), Ship::Anchored);
        assert_eq!(HeadState::Gone.ship(), Ship::Anchored);
    }

    /// The two drawings must be interchangeable in the head area, so they have
    /// to be the same block of lines at the same width.
    #[test]
    fn both_ships_are_the_same_size() {
        for art in [SAILING, ANCHORED] {
            assert_eq!(art.len(), 8);
            assert!(art
                .iter()
                .all(|line| line.chars().count() == usize::from(FIGUREHEAD_WIDTH)));
        }
    }

    /// Only the sailing ship moves; an anchored one must sit still rather than
    /// churning a wake it is not making.
    #[test]
    fn the_wake_only_runs_under_sail() {
        let sailing: Vec<_> = (0..4).map(|t| figurehead_frame(HeadState::Talking, t)).collect();
        assert!(sailing.windows(2).any(|pair| pair[0] != pair[1]));

        let resting = figurehead_frame(HeadState::Idle, 0);
        for tick in 0..8 {
            assert_eq!(figurehead_frame(HeadState::Idle, tick), resting);
        }
    }

    /// Whatever the state and however far the animation has run, the drawing
    /// keeps one shape and one width, so it never clips or jitters.
    #[test]
    fn every_frame_keeps_the_same_shape() {
        for state in ALL {
            for tick in 0..12 {
                let art = figurehead_frame(state, tick);
                assert_eq!(art.len(), SAILING.len());
                assert!(art
                    .iter()
                    .all(|line| line.chars().count() == usize::from(FIGUREHEAD_WIDTH)));
                assert!(
                    !art.concat().contains('W') && !art.concat().contains('P'),
                    "a cell was left unfilled in {art:?}"
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

    /// The notification ping hangs off this transition, so a session left
    /// idling has to keep reporting no change however long it sits there.
    #[test]
    fn settling_reports_the_transition_once_per_lull() {
        let mut head = Head::new();
        head.set_state(HeadState::Talking);
        assert!(head.settle(Duration::ZERO, false), "the lull begins");
        for _ in 0..5 {
            assert!(!head.settle(Duration::ZERO, false), "already idling");
        }
        head.set_state(HeadState::Talking);
        assert!(head.settle(Duration::ZERO, false), "a fresh lull reports again");
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
