//! Renders a `Decision` as a distinct on-screen box, separate from the
//! dimmed transcript region, with the currently highlighted choice tracked
//! by index.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::decision::Decision;

pub struct DecisionBox {
    pub decision: Decision,
    pub selected: usize,
}

impl DecisionBox {
    pub fn new(decision: Decision) -> Self {
        DecisionBox {
            decision,
            selected: 0,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.decision.options.len() {
            self.selected += 1;
        }
    }

    pub fn selected_option(&self) -> &str {
        &self.decision.options[self.selected]
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .decision
            .options
            .iter()
            .map(|opt| ListItem::new(Line::from(Span::raw(opt.clone()))))
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                format!(" decision: {} ", self.decision.prompt),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().fg(Color::Yellow));

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");

        let mut state = ListState::default();
        state.select(Some(self.selected));

        frame.render_stateful_widget(list, area, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::Decision;

    fn sample() -> Decision {
        Decision {
            prompt: "merge?".into(),
            options: vec![
                "yes".into(),
                "no".into(),
                "Something else".into(),
                "Chat about this".into(),
            ],
        }
    }

    #[test]
    fn move_down_stops_at_the_last_option() {
        let mut b = DecisionBox::new(sample());
        for _ in 0..10 {
            b.move_down();
        }
        assert_eq!(b.selected, 3);
        assert_eq!(b.selected_option(), "Chat about this");
    }

    #[test]
    fn move_up_stops_at_the_first_option() {
        let mut b = DecisionBox::new(sample());
        b.move_up();
        assert_eq!(b.selected, 0);
        assert_eq!(b.selected_option(), "yes");
    }

    #[test]
    fn navigation_round_trips_to_a_middle_option() {
        let mut b = DecisionBox::new(sample());
        b.move_down();
        b.move_down();
        b.move_up();
        assert_eq!(b.selected_option(), "no");
    }
}
