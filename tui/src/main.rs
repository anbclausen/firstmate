mod child;
mod config;
mod crew;
mod decision;
mod decision_box;
mod footer;
mod head;
mod keys;
mod loading;
mod tasks;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use tui_term::widget::{Cursor, PseudoTerminal};

use child::{Child, ChildEvent};
use config::Harness;
use crew::CrewPanel;
use decision_box::DecisionBox;
use footer::ContextUsage;
use head::{Head, HeadState};
use tasks::TasksPanel;

/// Pty size used before the first draw has told us what the pane is; the
/// real size is applied by `App::sync_size` on the very first frame.
const INITIAL_SIZE: (u16, u16) = (24, 80);

/// How much scrollback the emulator keeps behind the visible screen.
const SCROLLBACK: usize = 1000;

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

/// Which layer owns the next keystroke.
///
/// `Terminal` hands the harness everything, Ctrl+C included, which is the
/// whole point of the agent pane being a real terminal. That leaves the
/// TUI no ordinary key to quit on, so `PREFIX` below switches to
/// `Command`, where the TUI's own keys live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Terminal,
    Command,
}

/// The one chord the TUI keeps for itself: Ctrl+B, tmux-style. Pressing it
/// twice sends a literal Ctrl+B on to the harness. Documented in README.md.
const PREFIX: char = 'b';

fn is_prefix(key: KeyEvent) -> bool {
    key.code == KeyCode::Char(PREFIX) && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Whether a keystroke should end the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Continue,
    Quit,
}

struct App {
    mode: Mode,
    head: Head,
    decision: Option<DecisionBox>,
    /// The agent pane's terminal emulator: every byte the harness writes is
    /// parsed into this screen grid and rendered as a real terminal.
    parser: vt100::Parser,
    child: Option<Child>,
    focus: Focus,
    /// Exit code once the harness is gone, which turns the pane into a
    /// dead surface the ordinary keys can quit from.
    exited: Option<i32>,
    /// A short problem to show the captain in the status bar; cleared on
    /// the next keystroke.
    notice: Option<String>,
    harness: Option<Harness>,
    pty_size: (u16, u16),
    /// Left sidebar: the backlog, read from `data/backlog.md`.
    tasks: TasksPanel,
    /// Right sidebar: the crew, read from `podman ps`.
    crew: CrewPanel,
    /// Bottom-right context indicator. No real source is wired yet (see
    /// `footer::ContextUsage`), so it honestly renders `n/a`.
    context: ContextUsage,
}

impl App {
    fn new() -> Self {
        let (rows, cols) = INITIAL_SIZE;
        App {
            mode: Mode::ChooseHarness { selected: 0 },
            head: Head::new(),
            decision: None,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            child: None,
            focus: Focus::Terminal,
            exited: None,
            notice: None,
            harness: None,
            pty_size: INITIAL_SIZE,
            tasks: TasksPanel::new(),
            crew: CrewPanel::new(),
            context: ContextUsage::Unavailable,
        }
    }

    fn start_harness(&mut self, root: &std::path::Path, harness: Harness) {
        if let Err(err) = config::save_default_harness(root, harness) {
            self.notice = Some(format!("could not save the default harness: {err}"));
        }
        self.harness = Some(harness);
        let (rows, cols) = self.pty_size;
        match child::spawn(harness.command(), &[], rows, cols) {
            Ok(child) => {
                self.child = Some(child);
                self.head.set_state(HeadState::Idle);
            }
            Err(err) => {
                self.notice = Some(format!("could not launch {harness}: {err}"));
            }
        }
        self.mode = Mode::Running;
    }

    fn kill_child(&mut self) {
        if let Some(child) = &mut self.child {
            child.kill();
        }
    }

    /// Drains everything the harness has produced since the last frame.
    /// Returns whether anything changed, so an idle fleet doesn't redraw.
    fn poll_child(&mut self) -> bool {
        let mut events = Vec::new();
        if let Some(child) = &self.child {
            while let Ok(event) = child.events.try_recv() {
                events.push(event);
            }
        }
        let changed = !events.is_empty();
        for event in events {
            match event {
                ChildEvent::Output(bytes) => {
                    self.head.set_state(HeadState::Talking);
                    self.parser.process(&bytes);
                }
                ChildEvent::Decision(decision) => {
                    self.head.set_state(HeadState::Thinking);
                    self.decision = Some(DecisionBox::new(decision));
                    // The overlay owns the keyboard, so a half-entered
                    // prefix chord must not survive into it.
                    self.focus = Focus::Terminal;
                }
                ChildEvent::DecisionParseError(err) => {
                    self.notice = Some(format!("malformed decision payload: {err}"));
                }
                ChildEvent::Exited(code) => {
                    self.exited = Some(code);
                    self.head.set_state(HeadState::Idle);
                }
            }
        }
        changed
    }

    /// Keeps the pty and the emulator the same size as the pane on screen,
    /// so the harness lays itself out to what the captain actually sees.
    fn sync_size(&mut self, screen: Rect) {
        let pane = pane_rect(screen);
        let inner = Block::default().borders(Borders::ALL).inner(pane);
        let size = (inner.height, inner.width);
        if size.0 == 0 || size.1 == 0 || size == self.pty_size {
            return;
        }
        self.pty_size = size;
        self.parser.screen_mut().set_size(size.0, size.1);
        if let Some(child) = &mut self.child {
            if let Err(err) = child.resize(size.0, size.1) {
                self.notice = Some(format!("could not resize the harness: {err}"));
            }
        }
    }

    fn send_key(&mut self, key: KeyEvent) {
        let modes = keys::Modes::application_cursor(self.parser.screen().application_cursor());
        let Some(bytes) = keys::encode(key, modes) else {
            return;
        };
        if let Some(Err(err)) = self.child.as_mut().map(|c| c.write_input(&bytes)) {
            self.notice = Some(format!("could not reach the harness: {err}"));
        }
    }

    /// True once there is nothing left to type into, which is what makes
    /// the plain quit keys safe to reclaim.
    fn pane_is_dead(&self) -> bool {
        self.child.is_none() || self.exited.is_some()
    }
}

fn main() -> anyhow::Result<()> {
    let root = repo_root();

    // This binary only ever runs inside the runtime container; the host-side
    // `fm` launcher (tui/fm) builds the image and execs `podman run` into it.
    let mut app = App::new();
    let default_harness = config::load_default_harness(&root);
    let first_run = default_harness.is_none();
    app.harness = default_harness;

    // First-launch loading screen only; the podman image build itself already
    // happened in the host-side `fm` launcher before this containerized
    // process ever started.
    if first_run {
        show_first_run_loading_screen()?;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app, &root);
    app.kill_child();

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

    // The head animates on its own cadence; the pane redraws whenever the
    // harness writes or the captain types, so typing stays responsive
    // without redrawing a still screen.
    let tick_rate = Duration::from_millis(120);
    let mut last_tick = Instant::now();
    let mut dirty = true;

    // The sidebars are backed by external state read off the UI thread: the
    // backlog is a cheap timed file read here, while the crew's `podman ps`
    // runs on its own thread and reports over a channel, so neither the file
    // nor podman is ever touched per frame.
    app.tasks.set(tasks::load(root));
    let crew_rx = crew::spawn_monitor(Duration::from_secs(2));
    let backlog_refresh = Duration::from_millis(1500);
    let mut last_backlog = Instant::now();

    loop {
        if app.poll_child() {
            dirty = true;
        }

        while let Ok(result) = crew_rx.try_recv() {
            let changed = match result {
                Ok(crew) => app.crew.set(crew),
                Err(err) => app.crew.set_error(err),
            };
            dirty |= changed;
        }

        if last_backlog.elapsed() >= backlog_refresh {
            if app.tasks.set(tasks::load(root)) {
                dirty = true;
            }
            last_backlog = Instant::now();
        }

        if matches!(app.mode, Mode::Running) {
            let size = terminal.size()?;
            app.sync_size(Rect::new(0, 0, size.width, size.height));
        }

        if dirty {
            terminal.draw(|frame| draw(frame, app))?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    dirty = true;
                    if handle_key(app, key, root) == Step::Quit {
                        app.kill_child();
                        return Ok(());
                    }
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.head.advance();
            last_tick = Instant::now();
            dirty = true;
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent, root: &std::path::Path) -> Step {
    match &mut app.mode {
        Mode::ChooseHarness { selected } => {
            match key.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    if *selected + 1 < Harness::ALL.len() {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let chosen = Harness::ALL[*selected];
                    app.start_harness(root, chosen);
                }
                KeyCode::Esc | KeyCode::Char('q') => return Step::Quit,
                _ => {}
            }
            Step::Continue
        }
        Mode::Running => handle_running_key(app, key),
    }
}

fn handle_running_key(app: &mut App, key: KeyEvent) -> Step {
    app.notice = None;

    // The decision box is a modal overlay: while it is up it owns the
    // keyboard, so a choice can't be typed into the harness by accident.
    if let Some(decision) = &mut app.decision {
        match key.code {
            KeyCode::Up => decision.move_up(),
            KeyCode::Down => decision.move_down(),
            KeyCode::Enter => {
                let choice = decision.selected_option().to_string();
                app.decision = None;
                app.head.set_state(HeadState::Idle);
                app.notice = Some(format!("chose: {choice}"));
            }
            KeyCode::Esc => {
                app.decision = None;
                app.head.set_state(HeadState::Idle);
            }
            _ => {}
        }
        return Step::Continue;
    }

    match app.focus {
        Focus::Command => {
            if key.code == KeyCode::Char('q') {
                return Step::Quit;
            }
            // Scrolling the sidebars keeps command mode so a run of keys walks
            // the lists; the status bar names these while command mode is up.
            match key.code {
                KeyCode::Up => return command_stay(app, |a| a.tasks.scroll_up()),
                KeyCode::Down => return command_stay(app, |a| a.tasks.scroll_down()),
                KeyCode::PageUp => return command_stay(app, |a| a.crew.scroll_up()),
                KeyCode::PageDown => return command_stay(app, |a| a.crew.scroll_down()),
                _ => {}
            }
            // Ctrl+B twice is how a literal Ctrl+B gets through to the
            // harness; anything else just leaves command mode.
            if is_prefix(key) {
                app.send_key(key);
            }
            app.focus = Focus::Terminal;
            Step::Continue
        }
        Focus::Terminal => {
            if is_prefix(key) {
                app.focus = Focus::Command;
            } else if app.pane_is_dead() {
                let ctrl_c = key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl_c || matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    return Step::Quit;
                }
            } else {
                app.send_key(key);
            }
            Step::Continue
        }
    }
}

/// Run a command-mode sidebar action and stay in command mode, so a run of
/// scroll keys walks a list without re-entering the prefix chord each time.
fn command_stay(app: &mut App, action: impl FnOnce(&mut App)) -> Step {
    action(app);
    app.focus = Focus::Command;
    Step::Continue
}

const HEAD_HEIGHT: u16 = 8;
const SIDEBAR_WIDTH: u16 = 24;
const MIN_PANE_WIDTH: u16 = 30;

/// The full running layout: the face on top, then a middle row of
/// tasks sidebar | agent pane | crew sidebar, then the status line and the
/// model/context footer. The agent pane stays the centerpiece; on a terminal
/// too narrow to seat both sidebars and a usable pane, the sidebars collapse
/// and the pane spans the whole middle rather than corrupting the layout.
struct Areas {
    head: Rect,
    tasks: Option<Rect>,
    pane: Rect,
    crew: Option<Rect>,
    status: Rect,
    footer: Rect,
}

fn compute_layout(area: Rect) -> Areas {
    let [head, middle, status, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEAD_HEIGHT),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

    if area.width >= 2 * SIDEBAR_WIDTH + MIN_PANE_WIDTH {
        let [tasks, pane, crew] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(SIDEBAR_WIDTH),
                Constraint::Min(MIN_PANE_WIDTH),
                Constraint::Length(SIDEBAR_WIDTH),
            ])
            .areas(middle);
        Areas {
            head,
            tasks: Some(tasks),
            pane,
            crew: Some(crew),
            status,
            footer,
        }
    } else {
        Areas {
            head,
            tasks: None,
            pane: middle,
            crew: None,
            status,
            footer,
        }
    }
}

/// The agent pane rect. Shared by drawing and by the resize path so the pty is
/// always sized to the pane that is actually rendered.
fn pane_rect(area: Rect) -> Rect {
    compute_layout(area).pane
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
    let areas = compute_layout(frame.area());

    app.head.render(frame, areas.head);

    if let Some(tasks_area) = areas.tasks {
        app.tasks.render(frame, tasks_area);
    }
    if let Some(crew_area) = areas.crew {
        app.crew.render(frame, crew_area);
    }

    let screen = app.parser.screen();
    let title = match app.harness {
        Some(harness) => format!(" {harness} "),
        None => " agent ".to_string(),
    };
    // The captain's caret belongs to the harness, so it is hidden while
    // anything else owns the keyboard.
    let cursor_visible =
        !screen.hide_cursor() && app.decision.is_none() && app.focus == Focus::Terminal;
    let pane = PseudoTerminal::new(screen)
        .block(Block::default().borders(Borders::ALL).title(title))
        .cursor(Cursor::default().visibility(cursor_visible));
    frame.render_widget(pane, areas.pane);

    frame.render_widget(status_bar(app), areas.status);

    let [model_area, context_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .areas(areas.footer);
    footer::render_model(frame, model_area, app.harness);
    footer::render_context(frame, context_area, app.context);

    if let Some(decision) = &app.decision {
        let popup = centered_rect(60, 40, areas.pane);
        frame.render_widget(Clear, popup);
        decision.render(frame, popup);
    }
}

/// The one always-visible reminder of who owns the keyboard and how to get
/// out, since Ctrl+C now belongs to the harness.
fn status_bar(app: &App) -> Paragraph<'static> {
    let (text, style) = if let Some(notice) = &app.notice {
        (
            format!(" {notice} "),
            Style::default().fg(Color::Black).bg(Color::Red),
        )
    } else if let Some(code) = app.exited {
        (
            format!(" harness exited ({code}) - press q to quit "),
            Style::default().fg(Color::Black).bg(Color::Red),
        )
    } else if app.child.is_none() {
        (
            " no harness running - press q to quit ".to_string(),
            Style::default().fg(Color::Black).bg(Color::Red),
        )
    } else if app.decision.is_some() {
        (
            " decision - up/down to choose, enter to pick, esc to dismiss ".to_string(),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    } else {
        match app.focus {
            Focus::Command => (
                " command - q quits, up/down scroll tasks, pgup/pgdn scroll crew, ctrl+b sends ctrl+b, any other key returns ".to_string(),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Focus::Terminal => (
                " keys go to the harness (ctrl+c included) - ctrl+b then q to quit ".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        }
    };

    Paragraph::new(Line::from(text)).style(style)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Decision, SENTINEL};
    use ratatui::backend::TestBackend;

    fn running_app() -> App {
        let mut app = App::new();
        app.mode = Mode::Running;
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// The captain's blocker had two halves: Ctrl+C did nothing, and the
    /// app could not be closed. Ctrl+C must now belong to the harness.
    #[test]
    fn ctrl_c_does_not_quit_the_tui() {
        let mut app = running_app();
        // A live pane is what makes Ctrl+C the harness's key; without one
        // there would be nothing to forward it to.
        app.child = Some(child::spawn("cat", &[], 24, 80).unwrap());

        assert_eq!(handle_running_key(&mut app, ctrl('c')), Step::Continue);

        app.kill_child();
    }

    /// The other half: there is always a way out, and it is the documented
    /// one.
    #[test]
    fn the_prefix_chord_then_q_quits() {
        let mut app = running_app();
        assert_eq!(handle_running_key(&mut app, ctrl(PREFIX)), Step::Continue);
        assert_eq!(app.focus, Focus::Command);
        assert_eq!(handle_running_key(&mut app, key(KeyCode::Char('q'))), Step::Quit);
    }

    #[test]
    fn command_mode_returns_to_the_terminal_on_any_other_key() {
        let mut app = running_app();
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Terminal);
    }

    /// Once the harness is gone there is nothing to type into, so the
    /// ordinary keys are safe to reclaim as a second way out.
    #[test]
    fn plain_q_quits_only_once_the_pane_is_dead() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], 24, 80).unwrap());
        assert_eq!(handle_running_key(&mut app, key(KeyCode::Char('q'))), Step::Continue);

        app.exited = Some(0);
        assert_eq!(handle_running_key(&mut app, key(KeyCode::Char('q'))), Step::Quit);

        app.kill_child();
    }

    /// The decision box is modal, so a choice can't leak into the harness
    /// as keystrokes.
    #[test]
    fn the_decision_overlay_owns_the_keyboard_while_it_is_up() {
        let mut app = running_app();
        app.decision = Some(DecisionBox::new(Decision {
            prompt: "merge?".into(),
            options: vec!["yes".into(), "no".into()],
        }));

        handle_running_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.decision.as_ref().unwrap().selected_option(), "no");

        handle_running_key(&mut app, ctrl(PREFIX));
        assert_eq!(app.focus, Focus::Terminal, "prefix must not steal a decision keystroke");

        assert_eq!(handle_running_key(&mut app, key(KeyCode::Enter)), Step::Continue);
        assert!(app.decision.is_none());
    }

    /// A decision arriving mid-chord must cancel it, or the next plain key
    /// after the box is dismissed would still be read as a command.
    #[test]
    fn a_decision_cancels_a_half_entered_prefix_chord() {
        let mut app = running_app();
        app.child = Some(
            child::spawn(
                "sh",
                &[
                    "-c".into(),
                    format!(
                        "printf '{SENTINEL} {{\"prompt\":\"p\",\"options\":[\"yes\"]}}\\n'; sleep 30"
                    ),
                ],
                24,
                80,
            )
            .unwrap(),
        );

        handle_running_key(&mut app, ctrl(PREFIX));
        assert_eq!(app.focus, Focus::Command);

        let start = Instant::now();
        while app.decision.is_none() && start.elapsed() < Duration::from_secs(5) {
            app.poll_child();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.decision.is_some(), "harness never produced the decision");
        assert_eq!(app.focus, Focus::Terminal);

        handle_running_key(&mut app, key(KeyCode::Esc));
        assert!(app.decision.is_none());

        assert_eq!(handle_running_key(&mut app, key(KeyCode::Char('q'))), Step::Continue);
        assert_eq!(app.focus, Focus::Terminal);

        app.kill_child();
    }

    /// Dismissing a decision returns to the harness rather than killing
    /// the session, which the earlier slice did.
    #[test]
    fn escaping_a_decision_dismisses_it_without_quitting() {
        let mut app = running_app();
        app.decision = Some(DecisionBox::new(Decision {
            prompt: "merge?".into(),
            options: vec!["yes".into()],
        }));

        assert_eq!(handle_running_key(&mut app, key(KeyCode::Esc)), Step::Continue);
        assert!(app.decision.is_none());
    }

    /// Sizing the emulator to the pane is what stops the harness drawing
    /// to the wrong width; the border costs a row and a column each side.
    #[test]
    fn sync_size_matches_the_emulator_to_the_pane_inside_its_border() {
        let mut app = running_app();
        let pane = pane_rect(Rect::new(0, 0, 100, 40));

        app.sync_size(Rect::new(0, 0, 100, 40));

        assert_eq!(app.pty_size, (pane.height - 2, pane.width - 2));
        assert_eq!(app.parser.screen().size(), app.pty_size);
    }

    /// A pane too small to hold a single cell would make the emulator
    /// panic rather than simply render nothing.
    #[test]
    fn sync_size_ignores_a_pane_with_no_room_inside_its_border() {
        let mut app = running_app();
        app.sync_size(Rect::new(0, 0, 2, 11));
        assert_eq!(app.pty_size, INITIAL_SIZE);
    }

    /// The earlier slice printed harness output as plain text, so a
    /// full-screen harness's control sequences landed on screen as
    /// garbage. They have to become a rendered screen instead: positioned,
    /// coloured, and with no escape bytes left in the output.
    #[test]
    fn the_pane_renders_a_screen_rather_than_raw_escape_codes() {
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        let screen = Rect::new(0, 0, 40, 20);
        app.sync_size(screen);
        // Clear, jump to row 3 column 5, and print in red.
        app.parser.process(b"\x1b[2J\x1b[3;5H\x1b[31mahoy captain\x1b[0m");

        let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
        terminal.draw(|frame| draw_running(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        let rendered: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(
            rendered.contains("ahoy captain"),
            "expected the emulated screen, got {rendered:?}"
        );
        assert!(
            !rendered.contains('\u{1b}'),
            "escape bytes leaked into the rendered screen"
        );

        let pane = pane_rect(screen);
        let inner = Block::default().borders(Borders::ALL).inner(pane);
        let cell = &buffer[(inner.x + 4, inner.y + 2)];
        assert_eq!(cell.symbol(), "a", "cursor addressing was not honoured");
        // SGR 31 is palette slot 1, carried through as an indexed colour
        // so the captain's own terminal theme still applies.
        assert_eq!(
            cell.fg,
            Color::Indexed(1),
            "colour attributes were not carried over"
        );
    }

    /// The way out has to be on screen, because Ctrl+C no longer is one.
    #[test]
    fn the_status_bar_always_shows_the_quit_path() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], 24, 80).unwrap());

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| draw_running(frame, &app)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        app.kill_child();
        assert!(
            rendered.contains("ctrl+b then q to quit"),
            "expected the quit hint on screen, got {rendered:?}"
        );
    }

    /// End to end over a real pty: harness output is a byte stream the
    /// emulator renders, and the decision sentinel still has to be spotted
    /// in that same stream.
    #[test]
    fn harness_output_reaches_both_the_emulator_and_the_decision_scanner() {
        let mut app = running_app();
        let mut child = child::spawn("cat", &[], 24, 80).unwrap();
        child.write_input(b"hello\r").unwrap();
        child
            .write_input(br#"::firstmate-decision:: {"prompt":"p","options":["a"]}"#)
            .unwrap();
        child.write_input(b"\r").unwrap();
        app.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && app.decision.is_none() {
            app.poll_child();
            std::thread::sleep(Duration::from_millis(20));
        }
        app.kill_child();

        assert!(
            app.parser.screen().contents().contains("hello"),
            "emulator screen should hold the harness output, got {:?}",
            app.parser.screen().contents()
        );
        assert_eq!(
            app.decision.as_ref().map(|d| d.decision.prompt.as_str()),
            Some("p")
        );
    }

    fn render_to_string(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw_running(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Every region the captain expects has to be on screen at once: the face,
    /// both sidebars with their live data, the model chip, and the context
    /// indicator, all around the agent pane.
    #[test]
    fn the_full_layout_shows_every_region() {
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        app.tasks
            .set(tasks::parse_backlog("## In flight\n- [ ] tui-layout - build it (since 2026-07-27)\n"));
        app.crew.set(crew::parse_ps(
            "fm-h-tui-layout\trunning\tUp 2 minutes\tfirstmate.task=tui-layout\n",
        ));

        let rendered = render_to_string(&app, 100, 40);

        assert!(rendered.contains("tasks"), "tasks sidebar title missing");
        assert!(rendered.contains("crew"), "crew sidebar title missing");
        assert!(rendered.contains("tui-layout"), "task id missing");
        assert!(rendered.contains("model: claude"), "model chip missing");
        assert!(rendered.contains("context"), "context indicator missing");
        assert!(rendered.contains("n/a"), "context should read n/a with no source");
    }

    /// A terminal too narrow for two sidebars and a usable pane must collapse
    /// the sidebars and keep the pane, not panic or corrupt the layout.
    #[test]
    fn a_narrow_terminal_collapses_the_sidebars() {
        let wide = compute_layout(Rect::new(0, 0, 100, 40));
        assert!(wide.tasks.is_some() && wide.crew.is_some());

        let narrow = compute_layout(Rect::new(0, 0, 50, 20));
        assert!(narrow.tasks.is_none() && narrow.crew.is_none());
        // The pane keeps the whole middle width when the sidebars are gone.
        assert_eq!(narrow.pane.width, 50);

        // Rendering at a squeeze must not panic.
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        let _ = render_to_string(&app, 50, 20);
        let _ = render_to_string(&app, 8, 6);
    }

    /// Command mode scrolls the sidebars in place and only leaves for a key
    /// that is not a scroll key.
    #[test]
    fn command_mode_scrolls_the_sidebars_and_stays() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], 24, 80).unwrap());
        app.tasks.set(tasks::parse_backlog(
            "## Queued\n- [ ] a - one (since 2026-07-27)\n- [ ] b - two (since 2026-07-27)\n",
        ));

        handle_running_key(&mut app, ctrl(PREFIX));
        assert_eq!(app.focus, Focus::Command);
        assert_eq!(
            handle_running_key(&mut app, key(KeyCode::Down)),
            Step::Continue
        );
        assert_eq!(app.focus, Focus::Command, "a scroll key keeps command mode");

        // A non-scroll key returns to the terminal.
        handle_running_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Terminal);

        app.kill_child();
    }
}
