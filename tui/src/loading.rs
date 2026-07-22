//! First-launch and podman-image-build loading screen: ASCII-art ship plus a
//! progress bar. `run.sh` and `firstmate.Containerfile` at the repo root own
//! the actual podman pull/build flow this eventually hooks into; this module
//! only owns how the TUI presents that wait and its failure path.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

const SHIP_ART: &str = r#"
        |    |    |
       )_)  )_)  )_)
      )___))___))___)\
     )____)____)_____)\\
   _____|____|____|____\\\__
   \                   /
    \_________________/
"#;

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

        let ship = Paragraph::new(Text::from(SHIP_ART))
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

/// Result of a build/pull phase run to completion.
pub struct BuildOutcome {
    pub success: bool,
    pub log: Vec<String>,
}

/// Runs a build/pull command to completion, capturing its full combined
/// output. On failure the caller must dump `log` to stdout and crash rather
/// than swallowing it, per this TUI's loading-screen contract: a failed
/// image build is never a silent or partial-log failure.
pub fn run_build_command(program: &str, args: &[&str]) -> anyhow::Result<BuildOutcome> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut log = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            log.push(line?);
        }
    }
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines() {
            log.push(line?);
        }
    }

    let status = child.wait()?;
    Ok(BuildOutcome {
        success: status.success(),
        log,
    })
}

/// Dumps a failed build's full log to stdout and exits the process.
/// Called only after `run_build_command` reports failure; never swallows
/// the log in favor of a short summary.
pub fn crash_with_build_log(log: &[String]) -> ! {
    println!("firstmate TUI: podman image build/pull failed. Full log:");
    println!("----------------------------------------------------------");
    for line in log {
        println!("{line}");
    }
    println!("----------------------------------------------------------");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_command_reports_success_and_captures_output() {
        let outcome = run_build_command("sh", &["-c", "echo hello; echo world"]).unwrap();
        assert!(outcome.success);
        assert!(outcome.log.iter().any(|l| l == "hello"));
        assert!(outcome.log.iter().any(|l| l == "world"));
    }

    #[test]
    fn failing_command_reports_failure_but_still_captures_its_log() {
        let outcome =
            run_build_command("sh", &["-c", "echo before-failure; exit 1"]).unwrap();
        assert!(!outcome.success);
        assert!(outcome.log.iter().any(|l| l == "before-failure"));
    }
}
