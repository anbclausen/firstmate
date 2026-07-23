//! The captain-facing "head": a small animated ASCII-art region reflecting
//! what the wrapped agent is doing, so the captain can read state at a
//! glance without watching the dimmed transcript region.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadState {
    Idle,
    Thinking,
    Talking,
}

impl HeadState {
    /// Animation frames for this state, cycled by `tick`.
    /// Kept simple and small on purpose; new states just add a new arm here.
    fn frames(self) -> &'static [&'static str] {
        match self {
            HeadState::Idle => &["( o.o )", "( -.- )"],
            HeadState::Thinking => &["( o.O )", "( O.o )", "( ~.~ )"],
            HeadState::Talking => &["( o_o )", "( o_O )", "( O_o )"],
        }
    }

    fn color(self) -> Color {
        match self {
            HeadState::Idle => Color::Gray,
            HeadState::Thinking => Color::Yellow,
            HeadState::Talking => Color::Cyan,
        }
    }

    fn label(self) -> &'static str {
        match self {
            HeadState::Idle => "idling",
            HeadState::Thinking => "thinking",
            HeadState::Talking => "talking",
        }
    }
}

pub struct Head {
    state: HeadState,
    tick: usize,
}

impl Head {
    pub fn new() -> Self {
        Head {
            state: HeadState::Idle,
            tick: 0,
        }
    }

    pub fn set_state(&mut self, state: HeadState) {
        if state != self.state {
            self.state = state;
            self.tick = 0;
        }
    }

    /// Advance the animation by one frame; call this on a fixed timer.
    pub fn advance(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let frames = self.state.frames();
        let art = frames[self.tick % frames.len()];
        let text = Text::from(vec![
            Line::from(""),
            Line::from(art),
            Line::from(""),
            Line::from(self.state.label()),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("firstmate")
            .style(Style::default().fg(self.state.color()));
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(self.state.color()))
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

    #[test]
    fn every_state_has_at_least_one_frame() {
        for state in [HeadState::Idle, HeadState::Thinking, HeadState::Talking] {
            assert!(!state.frames().is_empty());
        }
    }
}
