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
    Sailing,
    /// The harness is gone; the mate has left the helm.
    Gone,
}

/// Which drawing a state gets: under way, or at rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ship {
    Sailing,
    Anchored,
}

/// One three-masted ship, drawn twice. The hull, deck and masts below are
/// character-for-character the same in both drawings: only the sails, the
/// anchor, the `P` masthead pennant and the `W` water differ, so the captain
/// reads a change of posture rather than a change of vessel. `P` and `W` are
/// single cells replaced in place, so the drawing keeps one shape as it moves.
const SAILING: [&str; 8] = [
    "        |    |P   |        ",
    "       )_)  )_)  )_)       ",
    "      )___))___))___)      ",
    "     )____)_____)____)     ",
    "   _____|____|____|_____   ",
    "   \\                   /   ",
    "    \\_________________/    ",
    "WWWWWWWWWWWWWWWWWWWWWWWWWWW",
];

/// The same ship with her sails furled on the yards and her anchor down on
/// its cable over the bow.
const ANCHORED: [&str; 8] = [
    "        |    |    |        ",
    "       _|_  _|_  _|_       ",
    "        |    |    |        ",
    "       _|_  _|_  _|_       ",
    "   _____|____|____|_____   ",
    "  |\\                   /   ",
    "   |\\_________________/    ",
    "WW\\_/WWWWWWWWWWWWWWWWWWWWWW",
];

/// Width of the drawings above, and the narrowest pane they fit in unclipped.
pub const FIGUREHEAD_WIDTH: u16 = 27;

impl HeadState {
    /// A session that is producing is under way; anything else is at rest.
    fn ship(self) -> Ship {
        match self {
            HeadState::Thinking | HeadState::Sailing => Ship::Sailing,
            HeadState::Idle | HeadState::Gone => Ship::Anchored,
        }
    }

    fn color(self) -> Color {
        match self {
            HeadState::Idle => Color::Gray,
            HeadState::Thinking => Color::Yellow,
            HeadState::Sailing => Color::Cyan,
            HeadState::Gone => Color::Red,
        }
    }

    fn label(self) -> &'static str {
        match self {
            HeadState::Idle => "idling",
            HeadState::Thinking => "thinking",
            HeadState::Sailing => "sailing",
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
/// Pure, so the state-to-art mapping is testable without a terminal, and
/// shared with the loading screen so first launch shows the same vessel.
pub fn figurehead_frame(state: HeadState, tick: usize) -> Vec<String> {
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

/// Whose turn the session is on.
///
/// The harness echoes the captain's own keystrokes back down the pty, so
/// output alone cannot tell work apart from an echo of what the captain just
/// typed. The turn can: it is the harness's only from the moment the captain
/// submits until the session comes to rest again, which is what makes the lull
/// at the end of it worth ringing at them. `Head::saw_output` answers the
/// narrower question of whether one piece of output was that echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Captain,
    Harness,
}

/// What a lull amounted to, since only one of the two is worth a ping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Still live, or already at rest: nothing to redraw.
    Unchanged,
    /// The ship came to rest, but on the captain's own turn - typing that the
    /// harness echoed back, not work it did.
    Quiet,
    /// The harness finished its turn and handed the keyboard back.
    YourTurn,
}

pub struct Head {
    state: HeadState,
    tick: usize,
    /// When the current state was last confirmed by something the session
    /// actually did; `settle` reads it to fall back to idling.
    since: Instant,
    turn: Turn,
    /// When the captain last sent a keystroke, so output arriving on its
    /// heels can be read as the harness echoing it back rather than working.
    last_key: Option<Instant>,
}

impl Head {
    pub fn new() -> Self {
        Head {
            state: HeadState::Idle,
            tick: 0,
            since: Instant::now(),
            // Nothing has been asked of the harness yet, so the first lull is
            // the captain's own, not a turn coming back to them.
            turn: Turn::Captain,
            last_key: None,
        }
    }

    /// The captain typed something that is not a submission. Whatever the
    /// harness echoes back is their own keystrokes, so the lull that follows
    /// is them pausing mid-sentence rather than the session handing back.
    pub fn captain_typed(&mut self) {
        self.turn = Turn::Captain;
        self.last_key = Some(Instant::now());
    }

    /// The captain submitted: from here the output is the harness's own work,
    /// and the lull at the end of it is the real "your turn, captain".
    pub fn captain_submitted(&mut self) {
        self.turn = Turn::Harness;
        self.last_key = Some(Instant::now());
    }

    /// The harness wrote something. That alone is not proof of work: it
    /// echoes the captain's keystrokes straight back down the pty, so output
    /// landing within `echo` of one is their own typing coming back. Such an
    /// echo may keep a ship that is already under way moving - the captain
    /// composing while the harness works must not becalm her - but it may
    /// never get one under way, which is what put an idle session under full
    /// sail while the captain was still writing.
    pub fn saw_output(&mut self, echo: Duration) {
        let echoed = self.last_key.is_some_and(|key| key.elapsed() < echo);
        if echoed && self.state != HeadState::Sailing {
            return;
        }
        self.set_state(HeadState::Sailing);
    }

    pub fn set_state(&mut self, state: HeadState) {
        self.since = Instant::now();
        if state != self.state {
            self.state = state;
            self.tick = 0;
        }
    }

    /// Fall back to idling once the session has been quiet for `quiet`.
    /// Sailing and thinking are both live states: neither is true any more
    /// once the harness has stopped producing anything. A pending decision is
    /// the exception - it produces no further output, but the session really is
    /// blocked on it, so it holds the state until the captain answers.
    ///
    /// Reports the lull once, so a settled fleet only redraws and only pings
    /// once, and reports it as the captain's turn only when the harness was
    /// the one working: the captain typing and pausing settles the ship
    /// without ringing at them for their own keystrokes.
    pub fn settle(&mut self, quiet: Duration, decision_pending: bool) -> Settled {
        let live = matches!(self.state, HeadState::Sailing | HeadState::Thinking);
        if !live || decision_pending || self.since.elapsed() < quiet {
            return Settled::Unchanged;
        }
        self.set_state(HeadState::Idle);
        // Handing back ends the harness's turn; the next one starts when the
        // captain submits again.
        match std::mem::replace(&mut self.turn, Turn::Captain) {
            Turn::Harness => Settled::YourTurn,
            Turn::Captain => Settled::Quiet,
        }
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
        HeadState::Sailing,
        HeadState::Gone,
    ];

    /// A head that has been handed a turn, which is what the captain
    /// submitting to the harness does.
    fn working() -> Head {
        let mut head = Head::new();
        head.captain_submitted();
        head.set_state(HeadState::Sailing);
        head
    }

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
        assert_eq!(HeadState::Sailing.ship(), Ship::Sailing);
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

    /// The two postures are one vessel: a change of state must read as the
    /// same ship taking in sail, not as a different ship sailing in.
    #[test]
    fn the_two_postures_draw_the_same_ship() {
        assert_eq!(SAILING[4], ANCHORED[4], "the deck is the same line in both");
        assert_eq!(
            masts(SAILING[4]),
            masts(ANCHORED[0]),
            "the masts stand in the same columns"
        );
        assert_eq!(masts(SAILING[0]), masts(ANCHORED[0]));
    }

    /// The columns a drawing's masts stand in.
    fn masts(line: &str) -> Vec<usize> {
        line.chars()
            .enumerate()
            .filter(|(_, c)| *c == '|')
            .map(|(col, _)| col)
            .collect()
    }

    /// Only the sailing ship moves; an anchored one must sit still rather than
    /// churning a wake it is not making.
    #[test]
    fn the_wake_only_runs_under_sail() {
        let sailing: Vec<_> = (0..4).map(|t| figurehead_frame(HeadState::Sailing, t)).collect();
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
        let mut head = working();
        assert_eq!(
            head.settle(Duration::from_secs(60), false),
            Settled::Unchanged,
            "still live"
        );
        assert_eq!(head.state, HeadState::Sailing);

        assert_eq!(head.settle(Duration::ZERO, false), Settled::YourTurn);
        assert_eq!(head.state, HeadState::Idle);
        assert_eq!(
            head.settle(Duration::ZERO, false),
            Settled::Unchanged,
            "idling is already settled"
        );
    }

    /// The notification ping hangs off this transition, so a session left
    /// idling has to keep reporting no change however long it sits there.
    #[test]
    fn settling_reports_the_transition_once_per_lull() {
        let mut head = working();
        assert_eq!(head.settle(Duration::ZERO, false), Settled::YourTurn, "the lull begins");
        for _ in 0..5 {
            assert_eq!(head.settle(Duration::ZERO, false), Settled::Unchanged, "already idling");
        }
        head.captain_submitted();
        head.set_state(HeadState::Sailing);
        assert_eq!(
            head.settle(Duration::ZERO, false),
            Settled::YourTurn,
            "a fresh turn reports again"
        );
    }

    /// The bug: the harness echoes what the captain types back as output, so
    /// typing and then pausing looked exactly like the harness finishing a
    /// turn and rang the ping at the captain for their own keystrokes.
    #[test]
    fn the_captains_own_typing_settles_without_taking_the_turn() {
        let mut head = Head::new();
        head.captain_typed();
        head.set_state(HeadState::Sailing);

        assert_eq!(
            head.settle(Duration::ZERO, false),
            Settled::Quiet,
            "the captain pausing mid-sentence is not their turn arriving"
        );
        assert_eq!(head.state, HeadState::Idle, "the ship still comes to rest");
    }

    /// A turn belongs to the harness only until it hands back, so more echoed
    /// keystrokes after it must not ring a second time.
    #[test]
    fn a_turn_is_handed_back_only_once() {
        let mut head = working();
        assert_eq!(head.settle(Duration::ZERO, false), Settled::YourTurn);

        head.captain_typed();
        head.set_state(HeadState::Sailing);
        assert_eq!(head.settle(Duration::ZERO, false), Settled::Quiet);
    }

    /// The bug: the harness echoes the captain's keystrokes straight back, so
    /// composing at an idle session drove the ship under full sail as if a
    /// turn were under way.
    #[test]
    fn an_echo_of_the_captains_typing_leaves_the_ship_at_anchor() {
        let mut head = Head::new();
        head.captain_typed();
        head.saw_output(Duration::from_secs(60));
        assert_eq!(head.state, HeadState::Idle);
    }

    /// Only the echo is held back. A session that really is working writes
    /// again once the echo has passed, and that gets her under way.
    #[test]
    fn output_that_is_not_an_echo_gets_the_ship_under_way() {
        let mut head = Head::new();
        head.captain_typed();
        head.saw_output(Duration::ZERO);
        assert_eq!(head.state, HeadState::Sailing);
    }

    /// The harness's own launch output arrives before the captain has typed
    /// anything at all, so there is nothing for it to be an echo of.
    #[test]
    fn output_before_the_captain_has_typed_is_work() {
        let mut head = Head::new();
        head.saw_output(Duration::from_secs(60));
        assert_eq!(head.state, HeadState::Sailing);
    }

    /// The captain composing a follow-up while the harness works must not
    /// becalm her: an echo may not get a ship under way, but it must keep the
    /// quiet timer of one already moving alive.
    #[test]
    fn typing_while_the_harness_works_keeps_her_under_way() {
        let mut head = working();
        std::thread::sleep(Duration::from_millis(200));
        head.captain_typed();
        head.saw_output(Duration::from_secs(60));

        assert_eq!(head.state, HeadState::Sailing);
        assert_eq!(
            head.settle(Duration::from_millis(100), false),
            Settled::Unchanged,
            "the echo kept her quiet timer alive"
        );
    }

    /// A decision box is a live blocked state, so the quiet timer must not
    /// flip the figurehead to idling while the captain is being asked.
    #[test]
    fn a_pending_decision_holds_the_thinking_state() {
        let mut head = Head::new();
        head.captain_submitted();
        head.set_state(HeadState::Thinking);
        assert_eq!(
            head.settle(Duration::ZERO, true),
            Settled::Unchanged,
            "a decision is still up"
        );
        assert_eq!(head.state, HeadState::Thinking);

        assert_eq!(
            head.settle(Duration::ZERO, false),
            Settled::YourTurn,
            "dismissed, so it settles"
        );
        assert_eq!(head.state, HeadState::Idle);
    }

    /// A dead harness is a fact, not a lull, so it must survive the quiet timer.
    #[test]
    fn a_gone_harness_does_not_settle_to_idling() {
        let mut head = Head::new();
        head.set_state(HeadState::Gone);
        assert_eq!(head.settle(Duration::ZERO, false), Settled::Unchanged);
        assert_eq!(head.state, HeadState::Gone);
    }
}
