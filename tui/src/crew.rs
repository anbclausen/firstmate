//! The right `crew` sidebar: firstmate's managed crewmate containers and their
//! health, read straight from `podman ps` in this TUI process. No agent ever
//! inspects or narrates a container; health is a programmatic read of the
//! container state podman reports.
//!
//! Crewmates are the containers carrying a `firstmate.task` label (see
//! `bin/backends/podman.sh`); the primary/TUI container carries
//! `firstmate.managed=true` but no task label, so filtering on the task label
//! selects exactly the crew. The `podman ps` call runs on a background thread
//! and reports over a channel, so the poll never blocks the UI or forks podman
//! per frame. The line parser is pure so it is testable without a live podman.

use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Working,
    Stalled,
    Stopped,
    Unknown,
}

impl Health {
    fn glyph(self) -> &'static str {
        match self {
            Health::Working => "+",
            Health::Stalled => "!",
            Health::Stopped => "x",
            Health::Unknown => "?",
        }
    }

    fn color(self) -> Color {
        match self {
            Health::Working => Color::Green,
            Health::Stalled => Color::Yellow,
            Health::Stopped => Color::Red,
            Health::Unknown => Color::DarkGray,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crewmate {
    /// The `firstmate.task` label value (or the container name as a fallback).
    pub task: String,
    pub health: Health,
    /// podman's human status string, e.g. `Up 3 minutes`.
    pub status: String,
}

/// Map a container's podman state (and status text as a fallback for podman
/// builds that leave `.State` empty in `ps`) onto crew health. "Working" is a
/// running container; anything paused, unhealthy, or not yet up is "stalled";
/// an exited or dead container is "stopped".
pub fn classify(state: &str, status: &str) -> Health {
    let status = status.to_lowercase();
    match state.trim().to_lowercase().as_str() {
        "running" => {
            if status.contains("unhealthy") {
                Health::Stalled
            } else {
                Health::Working
            }
        }
        "paused" => Health::Stalled,
        "exited" | "dead" | "stopped" | "stopping" | "removing" => Health::Stopped,
        "created" | "configured" | "initialized" => Health::Stalled,
        "" => classify_from_status(&status),
        _ => Health::Unknown,
    }
}

fn classify_from_status(status: &str) -> Health {
    if status.starts_with("up") {
        if status.contains("unhealthy") {
            Health::Stalled
        } else {
            Health::Working
        }
    } else if status.contains("paused") {
        Health::Stalled
    } else if status.starts_with("exited") || status.contains("dead") {
        Health::Stopped
    } else {
        Health::Unknown
    }
}

/// The Go-template `podman ps --format` this backend reads: name, state, human
/// status, then the whole label map (parsed here rather than via a per-version
/// `{{.Label ...}}` function, so the same output is readable across podman
/// builds).
pub const PS_FORMAT: &str = "{{.Names}}\t{{.State}}\t{{.Status}}\t{{.Labels}}";

/// Parse the tab-delimited `podman ps` output produced by `PS_FORMAT`.
pub fn parse_ps(output: &str) -> Vec<Crewmate> {
    let mut crew = Vec::new();
    for raw in output.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.splitn(4, '\t');
        let name = fields.next().unwrap_or("").trim();
        let state = fields.next().unwrap_or("").trim();
        let status = fields.next().unwrap_or("").trim();
        let labels = fields.next().unwrap_or("");

        let task = label_value(labels, "firstmate.task")
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| name.to_string());
        crew.push(Crewmate {
            task,
            health: classify(state, status),
            status: status.to_string(),
        });
    }
    crew
}

/// Extract a single label's value from podman's comma-joined `key=value` label
/// rendering, matching the key exactly so `firstmate.task` never picks up a
/// longer key that merely starts with it.
fn label_value(labels: &str, key: &str) -> Option<String> {
    labels.split(',').find_map(|part| {
        part.trim()
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .map(|value| value.to_string())
    })
}

/// One `podman ps` read. Errors (podman missing, machine down) come back as a
/// message the sidebar can show honestly rather than as a crash.
pub fn fetch() -> Result<Vec<Crewmate>, String> {
    let output = Command::new("podman")
        .args([
            "ps",
            "--all",
            "--filter",
            "label=firstmate.task",
            "--format",
            PS_FORMAT,
        ])
        .output()
        .map_err(|err| format!("podman unavailable: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("podman ps failed: {}", stderr.trim()));
    }
    Ok(parse_ps(&String::from_utf8_lossy(&output.stdout)))
}

/// Spawn a background poller that reads `podman ps` every `interval` and reports
/// each result over the returned channel. The thread exits when the receiver is
/// dropped, so it lives exactly as long as the UI wants it. The UI drains the
/// channel without blocking, and podman is forked once per interval, never per
/// frame.
pub fn spawn_monitor(interval: Duration) -> Receiver<Result<Vec<Crewmate>, String>> {
    let (tx, rx) = channel();
    thread::spawn(move || loop {
        if tx.send(fetch()).is_err() {
            return;
        }
        thread::sleep(interval);
    });
    rx
}

/// The right sidebar's view state: the latest crew snapshot, a scroll/highlight
/// cursor, and a note shown when podman could not be read.
pub struct CrewPanel {
    crew: Vec<Crewmate>,
    note: Option<String>,
    state: ListState,
}

impl CrewPanel {
    pub fn new() -> Self {
        CrewPanel {
            crew: Vec::new(),
            note: Some("loading...".to_string()),
            state: ListState::default(),
        }
    }

    /// Accept a fresh crew snapshot. Returns whether anything changed.
    pub fn set(&mut self, crew: Vec<Crewmate>) -> bool {
        if self.note.is_none() && crew == self.crew {
            return false;
        }
        self.crew = crew;
        self.note = None;
        self.clamp();
        true
    }

    /// Record that podman could not be read; the list is cleared so the sidebar
    /// never shows a stale roster as if it were current.
    pub fn set_error(&mut self, message: String) -> bool {
        if self.crew.is_empty() && self.note.as_deref() == Some(message.as_str()) {
            return false;
        }
        self.crew.clear();
        self.note = Some(message);
        self.state.select(None);
        true
    }

    fn clamp(&mut self) {
        if self.crew.is_empty() {
            self.state.select(None);
        } else {
            let last = self.crew.len() - 1;
            let selected = self.state.selected().unwrap_or(0).min(last);
            self.state.select(Some(selected));
        }
    }

    pub fn scroll_down(&mut self) {
        if self.crew.is_empty() {
            return;
        }
        let last = self.crew.len() - 1;
        let next = self.state.selected().map(|s| (s + 1).min(last)).unwrap_or(0);
        self.state.select(Some(next));
    }

    pub fn scroll_up(&mut self) {
        if self.crew.is_empty() {
            return;
        }
        let prev = self.state.selected().map(|s| s.saturating_sub(1)).unwrap_or(0);
        self.state.select(Some(prev));
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::ALL).title("crew");

        if self.crew.is_empty() {
            let text = self.note.clone().unwrap_or_else(|| "no crew".to_string());
            let placeholder = List::new(vec![ListItem::new(Line::from(Span::styled(
                text,
                Style::default().fg(Color::DarkGray),
            )))])
            .block(block);
            frame.render_widget(placeholder, area);
            return;
        }

        let items: Vec<ListItem> = self.crew.iter().map(crew_item).collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("");

        let mut state = self.state.clone();
        frame.render_stateful_widget(list, area, &mut state);
    }
}

impl Default for CrewPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn crew_item(crew: &Crewmate) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::styled(
            format!("{} ", crew.health.glyph()),
            Style::default().fg(crew.health.color()),
        ),
        Span::styled(
            crew.task.clone(),
            Style::default()
                .fg(crew.health.color())
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    ListItem::new(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_states_into_health() {
        assert_eq!(classify("running", "Up 3 minutes"), Health::Working);
        assert_eq!(
            classify("running", "Up 3 minutes (unhealthy)"),
            Health::Stalled
        );
        assert_eq!(classify("paused", "Paused"), Health::Stalled);
        assert_eq!(classify("exited", "Exited (0) 1 minute ago"), Health::Stopped);
        assert_eq!(classify("created", "Created"), Health::Stalled);
        assert_eq!(classify("weird", "?"), Health::Unknown);
    }

    #[test]
    fn falls_back_to_status_when_state_is_empty() {
        assert_eq!(classify("", "Up 10 seconds"), Health::Working);
        assert_eq!(classify("", "Exited (137) 2 seconds ago"), Health::Stopped);
        assert_eq!(classify("", ""), Health::Unknown);
    }

    #[test]
    fn parses_ps_rows_into_crew() {
        let output = "\
fm-ab12-tui-layout\trunning\tUp 5 minutes\tfirstmate.managed=true,firstmate.home=ab12,firstmate.task=tui-layout
fm-ab12-crew-health\texited\tExited (0) 1 minute ago\tfirstmate.managed=true,firstmate.task=crew-health
";
        let crew = parse_ps(output);
        assert_eq!(crew.len(), 2);
        assert_eq!(crew[0].task, "tui-layout");
        assert_eq!(crew[0].health, Health::Working);
        assert_eq!(crew[1].task, "crew-health");
        assert_eq!(crew[1].health, Health::Stopped);
    }

    #[test]
    fn matches_the_task_label_exactly() {
        // A longer key that merely starts with the sought key must not match.
        let labels = "firstmate.task-note=nope,firstmate.task=real";
        assert_eq!(label_value(labels, "firstmate.task").as_deref(), Some("real"));
    }

    #[test]
    fn falls_back_to_the_container_name_without_a_task_label() {
        let output = "fm-ab12-mystery\trunning\tUp 1 minute\tfirstmate.managed=true\n";
        let crew = parse_ps(output);
        assert_eq!(crew[0].task, "fm-ab12-mystery");
    }

    #[test]
    fn empty_output_is_no_crew() {
        assert!(parse_ps("").is_empty());
        assert!(parse_ps("\n  \n").is_empty());
    }

    #[test]
    fn set_error_clears_the_roster_and_reports_change_once() {
        let mut panel = CrewPanel::new();
        assert!(panel.set(vec![Crewmate {
            task: "a".into(),
            health: Health::Working,
            status: "Up".into(),
        }]));
        assert!(panel.set_error("podman unavailable".into()));
        assert!(!panel.set_error("podman unavailable".into()));
    }

    #[test]
    fn set_reports_no_change_for_identical_snapshots() {
        let mut panel = CrewPanel::new();
        let snap = vec![Crewmate {
            task: "a".into(),
            health: Health::Working,
            status: "Up".into(),
        }];
        // First set always changes (clears the initial loading note).
        assert!(panel.set(snap.clone()));
        assert!(!panel.set(snap));
    }
}
