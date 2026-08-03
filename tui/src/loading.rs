//! First-launch loading screen: ASCII-art ship plus a progress bar. The
//! podman image build happens in the host-side `fm` launcher before this
//! containerized process starts, so this module only owns how the TUI
//! presents the brief first-run setup wait.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::head::{figurehead_frame, HeadState};

/// The figurehead's own ship, so first launch and the running TUI show one
/// vessel rather than two drawings that drift apart. A single frame: nothing
/// is under way yet to animate.
fn ship_art() -> String {
    figurehead_frame(HeadState::Sailing, 0).join("\n")
}

pub struct LoadingScreen {
    pub label: String,
    pub progress: u16, // 0..=100
}

impl LoadingScreen {
    pub fn new(label: impl Into<String>) -> Self {
        LoadingScreen {
            label: label.into(),
            progress: 0,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(9),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(area);

        let ship = Paragraph::new(Text::from(ship_art()))
            .alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().fg(Color::Cyan));
        frame.render_widget(ship, chunks[0]);

        let label = Paragraph::new(Line::from(self.label.as_str()))
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(label, chunks[1]);

        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("loading"))
            .gauge_style(Style::default().fg(Color::Green))
            .percent(self.progress);
        frame.render_widget(gauge, chunks[2]);
    }
}
