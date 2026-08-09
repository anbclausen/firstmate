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
//! per frame. The output parser is pure so it is testable without a live podman.

use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

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
    /// The container's own name, which is what `podman exec` addresses; the
    /// task label is a display name and is not always the container's.
    pub name: String,
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

/// One container as `podman ps --format json` reports it. The JSON form is read
/// rather than a Go template because podman renders `{{.Labels}}` as a Go map
/// literal, not as docker's comma-joined `key=value` list, while the JSON
/// `Labels` object is stable across podman builds.
#[derive(Debug, Deserialize)]
struct PsEntry {
    #[serde(rename = "Names", default)]
    names: Option<Vec<String>>,
    #[serde(rename = "State", default)]
    state: Option<String>,
    #[serde(rename = "Status", default)]
    status: Option<String>,
    #[serde(rename = "Labels", default)]
    labels: Option<HashMap<String, String>>,
}

/// Parse `podman ps --format json` output. A read podman could not produce
/// well-formed JSON for is an error the sidebar reports rather than an empty
/// roster it would show as if the crew were gone.
pub fn parse_ps(json: &str) -> Result<Vec<Crewmate>, String> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<PsEntry> =
        serde_json::from_str(json).map_err(|err| format!("podman ps unreadable: {err}"))?;

    Ok(entries
        .into_iter()
        .map(|entry| {
            let state = entry.state.unwrap_or_default();
            let status = entry.status.unwrap_or_default();
            let name = entry
                .names
                .as_ref()
                .and_then(|names| names.first().cloned())
                .unwrap_or_default();
            let task = entry
                .labels
                .as_ref()
                .and_then(|labels| labels.get("firstmate.task"))
                .map(|task| task.trim().to_string())
                .filter(|task| !task.is_empty())
                .unwrap_or_else(|| name.clone());
            Crewmate {
                health: classify(state.trim(), status.trim()),
                task,
                name,
                status: status.trim().to_string(),
            }
        })
        .collect())
}

/// Why the TUI is joining a crewmate's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    /// Watching over the crewmate's shoulder: the client is read-only, so a
    /// stray keystroke can never reach a crewmate's harness.
    Preview,
    /// The captain taking the keyboard, which is an ordinary tmux client.
    Attach,
}

/// The tmux session each crewmate container runs its harness in.
///
/// `bin/backends/podman.sh` creates it and owns the name; reading the same
/// environment variable it does is what keeps a fleet that renamed the session
/// reachable from here.
fn tmux_session() -> String {
    std::env::var("FM_BACKEND_PODMAN_TMUX_SESSION")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "work".to_string())
}

/// The command that joins `container`'s crewmate session, to be run on a pty of
/// its own by `child::spawn`.
///
/// A preview client also asks tmux not to let its size count, because the
/// popup is far smaller than the pane and a client that counts would reflow
/// the crewmate's own screen down to the size of the captain's peek. That flag
/// is tmux 3.2 and newer, so a container carrying an older tmux falls back to
/// an ordinary read-only client rather than failing to attach at all. The
/// session name is passed as an argument rather than interpolated, so a name
/// carrying shell metacharacters stays one word.
pub fn session_command(container: &str, join: Join) -> (String, Vec<String>) {
    let session = tmux_session();
    let mut args: Vec<String> = ["exec", "-it", container]
        .iter()
        .map(|a| a.to_string())
        .collect();
    match join {
        Join::Preview => args.extend([
            "sh".to_string(),
            "-c".to_string(),
            "tmux attach -r -f ignore-size -t \"$1\" 2>/dev/null || exec tmux attach -r -t \"$1\""
                .to_string(),
            "fm-tui".to_string(),
            session,
        ]),
        Join::Attach => args.extend([
            "tmux".to_string(),
            "attach".to_string(),
            "-t".to_string(),
            session,
        ]),
    }
    ("podman".to_string(), args)
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
            "json",
        ])
        .output()
        .map_err(|err| format!("podman unavailable: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("podman ps failed: {}", stderr.trim()));
    }
    parse_ps(&String::from_utf8_lossy(&output.stdout))
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

    /// The crewmate under the cursor, if the roster has one.
    pub fn selected(&self) -> Option<&Crewmate> {
        self.state.selected().and_then(|i| self.crew.get(i))
    }

    pub fn select_next(&mut self) {
        if self.crew.is_empty() {
            return;
        }
        let last = self.crew.len() - 1;
        let next = self.state.selected().map(|s| (s + 1).min(last)).unwrap_or(0);
        self.state.select(Some(next));
    }

    pub fn select_prev(&mut self) {
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

    const PS_JSON: &str = r#"[
  {
    "Names": ["fm-ab12-tui-layout"],
    "State": "running",
    "Status": "Up 5 minutes",
    "Labels": {
      "firstmate.managed": "true",
      "firstmate.home": "ab12",
      "firstmate.task-note": "nope",
      "firstmate.task": "tui-layout"
    }
  },
  {
    "Names": ["fm-ab12-crew-health"],
    "State": "exited",
    "Status": "Exited (0) 1 minute ago",
    "Labels": {"firstmate.managed": "true", "firstmate.task": "crew-health"}
  }
]"#;

    #[test]
    fn parses_ps_json_into_crew() {
        let crew = parse_ps(PS_JSON).unwrap();
        assert_eq!(crew.len(), 2);
        assert_eq!(crew[0].task, "tui-layout");
        assert_eq!(
            crew[0].name, "fm-ab12-tui-layout",
            "the container name is what podman exec addresses"
        );
        assert_eq!(crew[0].health, Health::Working);
        assert_eq!(crew[0].status, "Up 5 minutes");
        assert_eq!(crew[1].task, "crew-health");
        assert_eq!(crew[1].health, Health::Stopped);
    }

    #[test]
    fn falls_back_to_the_container_name_without_a_task_label() {
        let json = r#"[{"Names":["fm-ab12-mystery"],"State":"running","Status":"Up 1 minute","Labels":{"firstmate.managed":"true"}}]"#;
        let crew = parse_ps(json).unwrap();
        assert_eq!(crew[0].task, "fm-ab12-mystery");
    }

    #[test]
    fn tolerates_a_null_label_map_and_missing_fields() {
        let json = r#"[{"Names":["fm-ab12-bare"],"Labels":null}]"#;
        let crew = parse_ps(json).unwrap();
        assert_eq!(crew[0].task, "fm-ab12-bare");
        assert_eq!(crew[0].health, Health::Unknown);
    }

    #[test]
    fn empty_output_is_no_crew() {
        assert!(parse_ps("").unwrap().is_empty());
        assert!(parse_ps("  \n").unwrap().is_empty());
        assert!(parse_ps("[]").unwrap().is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_not_an_empty_roster() {
        assert!(parse_ps("not json").is_err());
    }

    /// A preview must never be able to type at a crewmate's harness, and must
    /// not drag the crewmate's own screen down to the size of the popup.
    #[test]
    fn the_preview_client_is_read_only_and_does_not_resize_the_crewmate() {
        let (program, args) = session_command("fm-ab12-one", Join::Preview);
        assert_eq!(program, "podman");
        let script = args.join(" ");
        assert!(script.contains("exec -it fm-ab12-one"), "{script}");
        assert!(script.contains("attach -r -f ignore-size"), "{script}");
        // An older tmux without the flag still has to get a read-only client.
        assert!(script.contains("|| exec tmux attach -r"), "{script}");
        // The session name is its own argument, never spliced into the script.
        assert_eq!(args.last().map(String::as_str), Some("work"));
    }

    /// Boarding is the captain taking the keyboard, so it is an ordinary
    /// read-write client and its size is meant to count.
    #[test]
    fn the_attach_client_is_an_ordinary_read_write_tmux_client() {
        let (_, args) = session_command("fm-ab12-one", Join::Attach);
        assert_eq!(
            args,
            vec!["exec", "-it", "fm-ab12-one", "tmux", "attach", "-t", "work"]
        );
    }

    #[test]
    fn set_error_clears_the_roster_and_reports_change_once() {
        let mut panel = CrewPanel::new();
        assert!(panel.set(vec![Crewmate {
            task: "a".into(),
            name: "fm-ab12-a".into(),
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
            name: "fm-ab12-a".into(),
            health: Health::Working,
            status: "Up".into(),
        }];
        // First set always changes (clears the initial loading note).
        assert!(panel.set(snap.clone()));
        assert!(!panel.set(snap));
    }
}
