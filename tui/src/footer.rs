//! The bottom footer row: the current model/harness chip on the left and a
//! context-usage indicator on the right. The bar and label builders are pure so
//! they are testable without a terminal.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::config::Harness;

/// How much of the wrapped agent's context window is used.
///
/// The wrapped harness runs in a pty and does not report its context budget to
/// this TUI, so there is no honest number to show yet: the indicator renders
/// `n/a`. WIRING POINT: when a real source exists (a status line the harness
/// emits on the decision channel, a sidecar file, or a future harness API),
/// construct `ContextUsage::Known(percent)` from it and store it on `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextUsage {
    Unavailable,
    /// Not yet constructed outside tests: this is the wiring point a real
    /// context source will fill in (see the type-level note above).
    #[allow(dead_code)]
    Known(u8),
}

impl ContextUsage {
    /// The percentage text, or `n/a` when no source is wired.
    pub fn label(self) -> String {
        match self {
            ContextUsage::Unavailable => "n/a".to_string(),
            ContextUsage::Known(percent) => format!("{}%", percent.min(100)),
        }
    }

    /// A fixed-width `[####----]` bar. An unavailable reading renders as an
    /// all-dashes bar so the widget stays honest rather than implying zero.
    pub fn bar(self, cells: usize) -> String {
        match self {
            ContextUsage::Unavailable => format!("[{}]", "-".repeat(cells)),
            ContextUsage::Known(percent) => {
                let filled = (usize::from(percent.min(100)) * cells + 50) / 100;
                let empty = cells.saturating_sub(filled);
                format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
            }
        }
    }

    fn color(self) -> Color {
        match self {
            ContextUsage::Unavailable => Color::DarkGray,
            ContextUsage::Known(p) if p >= 90 => Color::Red,
            ContextUsage::Known(p) if p >= 75 => Color::Yellow,
            ContextUsage::Known(_) => Color::Green,
        }
    }
}

/// The bottom-left chip text, e.g. `model: claude`.
pub fn model_chip_text(harness: Option<Harness>) -> String {
    let name = harness
        .map(|h| h.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!("model: {name}")
}

pub fn render_model(frame: &mut Frame, area: Rect, harness: Option<Harness>) {
    let chip = Paragraph::new(Line::from(Span::styled(
        format!(" {} ", model_chip_text(harness)),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Left);
    frame.render_widget(chip, area);
}

/// The right-aligned indicator text, sized to the room available. The reading
/// itself is what matters, so a footer too narrow for a legible bar drops the
/// bar rather than letting the right-aligned paragraph clip the percentage.
pub fn context_text(usage: ContextUsage, width: usize) -> String {
    let label = usage.label();
    let fixed = "context ".len() + label.len() + 2;
    let cells = width.saturating_sub(fixed + 2);
    if cells < 4 {
        format!("context {label} ")
    } else {
        format!("context {} {label} ", usage.bar(cells.min(20)))
    }
}

pub fn render_context(frame: &mut Frame, area: Rect, usage: ContextUsage) {
    let text = context_text(usage, usize::from(area.width));
    let widget = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(usage.color()),
    )))
    .alignment(Alignment::Right);
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_context_reads_as_na() {
        assert_eq!(ContextUsage::Unavailable.label(), "n/a");
        assert_eq!(ContextUsage::Unavailable.bar(8), "[--------]");
    }

    #[test]
    fn known_context_fills_the_bar_proportionally() {
        assert_eq!(ContextUsage::Known(50).label(), "50%");
        assert_eq!(ContextUsage::Known(50).bar(8), "[####----]");
        assert_eq!(ContextUsage::Known(0).bar(8), "[--------]");
        assert_eq!(ContextUsage::Known(100).bar(8), "[########]");
    }

    #[test]
    fn context_percentage_is_clamped() {
        assert_eq!(ContextUsage::Known(200).label(), "100%");
        assert_eq!(ContextUsage::Known(200).bar(4), "[####]");
    }

    #[test]
    fn a_narrow_footer_drops_the_bar_and_keeps_the_reading() {
        assert_eq!(context_text(ContextUsage::Unavailable, 18), "context n/a ");
        assert_eq!(context_text(ContextUsage::Known(50), 8), "context 50% ");
    }

    #[test]
    fn a_wide_footer_fits_bar_label_and_padding() {
        let text = context_text(ContextUsage::Unavailable, 40);
        assert_eq!(text, "context [--------------------] n/a ");
        assert!(text.len() <= 40);

        let text = context_text(ContextUsage::Unavailable, 24);
        assert_eq!(text, "context [---------] n/a ");
        assert!(text.len() <= 24);
    }

    #[test]
    fn model_chip_names_the_harness_or_a_dash() {
        assert_eq!(model_chip_text(Some(Harness::Claude)), "model: claude");
        assert_eq!(model_chip_text(None), "model: -");
    }
}
