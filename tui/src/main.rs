mod child;
mod config;
mod decision;
mod decision_box;
mod head;
mod loading;

use std::env;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;

use child::ChildEvent;
use config::Harness;
use decision_box::DecisionBox;
use head::{Head, HeadState};

fn repo_root() -> PathBuf {
    // This binary lives at <repo>/tui; walk up from the crate manifest dir
    // at compile time so it works regardless of the process's cwd.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tui/ crate has a parent repo root")
        .to_path_buf()
}

enum Mode {
    ChooseHarness { selected: usize },
    Running,
}

struct App {
    mode: Mode,
    head: Head,
    transcript: Vec<String>,
    decision: Option<DecisionBox>,
    child_rx: Option<std::sync::mpsc::Receiver<ChildEvent>>,
    child_killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    harness: Option<Harness>,
}

impl App {
    fn new() -> Self {
        App {
            mode: Mode::ChooseHarness { selected: 0 },
            head: Head::new(),
            transcript: Vec::new(),
            decision: None,
            child_rx: None,
            child_killer: None,
            harness: None,
        }
    }

    fn start_harness(&mut self, root: &std::path::Path, harness: Harness) {
        if let Err(err) = config::save_default_harness(root, harness) {
            self.transcript
                .push(format!("failed to save default harness: {err}"));
        }
        self.harness = Some(harness);
        match child::spawn(harness.command(), &[]) {
            Ok((rx, killer)) => {
                self.child_rx = Some(rx);
                self.child_killer = Some(killer);
                self.head.set_state(HeadState::Idle);
            }
            Err(err) => {
                self.transcript.push(format!("failed to launch {harness}: {err}"));
            }
        }
        self.mode = Mode::Running;
    }

    /// Terminates the wrapped harness process, if one is running, so it
    /// doesn't keep running detached after the TUI exits.
    fn kill_child(&mut self) {
        if let Some(killer) = &mut self.child_killer {
            let _ = killer.kill();
        }
    }

    fn poll_child(&mut self) {
        let Some(rx) = &self.child_rx else { return };
        while let Ok(event) = rx.try_recv() {
            match event {
                ChildEvent::Line(line) => {
                    self.head.set_state(HeadState::Talking);
                    self.transcript.push(line);
                }
                ChildEvent::Decision(decision) => {
                    self.head.set_state(HeadState::Thinking);
                    self.decision = Some(DecisionBox::new(decision));
                }
                ChildEvent::DecisionParseError(err) => {
                    self.transcript
                        .push(format!("[malformed decision payload: {err}]"));
                }
                ChildEvent::Exited(code) => {
                    self.transcript.push(format!("[harness exited: {code}]"));
                    self.head.set_state(HeadState::Idle);
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let root = repo_root();
    let mut app = App::new();
    let default_harness = config::load_default_harness(&root);
    let first_run = default_harness.is_none();
    app.harness = default_harness;

    // Loading screen: shown on first launch, and whenever the environment
    // asks for a podman image build/pull to run first (see loading.rs and
    // run.sh / firstmate.Containerfile for the container boot flow this
    // eventually hooks into). A failed build/pull crashes with the full log
    // on stdout rather than swallowing it.
    if let Ok(build_cmd) = env::var("FM_TUI_PODMAN_BUILD") {
        let mut parts = build_cmd.split_whitespace();
        if let Some(program) = parts.next() {
            let args: Vec<&str> = parts.collect();
            println!("firstmate TUI: building/pulling podman image...");
            let outcome = loading::run_build_command(program, &args)?;
            if !outcome.success {
                loading::crash_with_build_log(&outcome.log);
            }
        }
    } else if first_run {
        show_first_run_loading_screen()?;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app, &root);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn show_first_run_loading_screen() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut screen = loading::LoadingScreen::new("welcome aboard, captain - setting up the first slice");
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(900) {
        screen.progress = ((start.elapsed().as_millis() * 100) / 900).min(100) as u16;
        terminal.draw(|frame| screen.render(frame, frame.area()))?;
        std::thread::sleep(Duration::from_millis(30));
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    // Skip straight to running if a default harness was already chosen on
    // a prior run, per the first-run-only prompt requirement.
    if let Some(harness) = app.harness {
        if matches!(app.mode, Mode::ChooseHarness { .. }) {
            app.start_harness(root, harness);
        }
    }

    let tick_rate = Duration::from_millis(150);
    let mut last_tick = Instant::now();

    loop {
        app.poll_child();

        terminal.draw(|frame| draw(frame, app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match &mut app.mode {
                    Mode::ChooseHarness { selected } => match key.code {
                        KeyCode::Up => {
                            *selected = selected.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            if *selected + 1 < Harness::ALL.len() {
                                *selected += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let chosen = Harness::ALL[*selected];
                            app.start_harness(root, chosen);
                        }
                        KeyCode::Esc | KeyCode::Char('q') => {
                            app.kill_child();
                            return Ok(());
                        }
                        _ => {}
                    },
                    Mode::Running => {
                        if let Some(decision) = &mut app.decision {
                            match key.code {
                                KeyCode::Up => decision.move_up(),
                                KeyCode::Down => decision.move_down(),
                                KeyCode::Enter => {
                                    let choice = decision.selected_option().to_string();
                                    app.transcript.push(format!("[decision: {choice}]"));
                                    app.decision = None;
                                    app.head.set_state(HeadState::Idle);
                                }
                                KeyCode::Esc => {
                                    app.kill_child();
                                    return Ok(());
                                }
                                _ => {}
                            }
                        } else if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
                            app.kill_child();
                            return Ok(());
                        }
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.head.advance();
            last_tick = Instant::now();
        }
    }
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    match &app.mode {
        Mode::ChooseHarness { selected } => draw_choose_harness(frame, *selected),
        Mode::Running => draw_running(frame, app),
    }
}

fn draw_choose_harness(frame: &mut ratatui::Frame, selected: usize) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);

    let title = Paragraph::new(Text::from("Pick a default harness for the firstmate TUI"))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = Harness::ALL
        .iter()
        .map(|h| ListItem::new(Line::from(h.command())))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("harness"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::White))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, chunks[1], &mut state);
}

fn draw_running(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(3)])
        .split(area);

    app.head.render(frame, chunks[0]);

    let transcript_text: Vec<Line> = app
        .transcript
        .iter()
        .rev()
        .take(chunks[1].height.saturating_sub(2) as usize)
        .rev()
        .map(|l| Line::from(l.as_str()))
        .collect();
    let transcript = Paragraph::new(Text::from(transcript_text))
        .style(Style::default().fg(Color::DarkGray))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("transcript (dimmed)"),
        );
    frame.render_widget(transcript, chunks[1]);

    if let Some(decision) = &app.decision {
        let popup = centered_rect(60, 40, area);
        frame.render_widget(ratatui::widgets::Clear, popup);
        decision.render(frame, popup);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
