mod child;
mod config;
mod crew;
mod decision;
mod decision_box;
mod head;
mod keys;
mod loading;
mod ping;
mod tasks;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use tui_term::widget::{Cursor, PseudoTerminal};

use child::{Child, ChildEvent};
use config::Harness;
use crew::{CrewPanel, Crewmate, Join};
use decision_box::DecisionBox;
use head::{Head, HeadState, Settled};
use tasks::TasksPanel;

/// Pty size used before the first draw has told us what the pane is; the
/// real size is applied by `App::sync_size` on the very first frame.
const INITIAL_SIZE: (u16, u16) = (24, 80);

/// How much scrollback the emulator keeps behind the visible screen.
const SCROLLBACK: usize = 1000;

/// How long the session has to stay quiet before the figurehead stops
/// claiming the harness is still talking or thinking.
const HEAD_QUIET: Duration = Duration::from_millis(600);

/// How long after a keystroke the harness's output is still that keystroke
/// coming back rather than work of its own. Generous, because a session that
/// really is working writes again a frame later and gets under way then.
const HEAD_ECHO: Duration = Duration::from_millis(250);

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

/// Whether a keystroke hands the turn to the harness. A plain Enter submits
/// what the captain typed; Shift+Enter only opens a new line in the harness's
/// input, so it is still the captain's turn (`keys.rs`).
fn is_submit(key: KeyEvent) -> bool {
    key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// Whether a keystroke should end the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Continue,
    Quit,
}

/// A crewmate's own session, joined over a pty of its own: either the live
/// read-only preview raised while the captain walks the crew, or the session
/// they attached into, which then owns the agent pane.
///
/// Firstmate's own session is never one of these. It keeps its own child and
/// its own emulator, which is what lets the captain attach to a crewmate
/// without firstmate's session being torn down behind them and come straight
/// back to it, scrollback and all.
struct CrewSession {
    /// The crewmate this session belongs to, so a moved cursor can tell
    /// whether what is on screen is still the right crewmate.
    crew: String,
    child: Child,
    parser: vt100::Parser,
}

impl CrewSession {
    fn open(
        crew: &str,
        program: &str,
        args: &[String],
        rows: u16,
        cols: u16,
    ) -> anyhow::Result<Self> {
        Ok(CrewSession {
            crew: crew.to_string(),
            child: child::spawn(program, args, None, rows, cols)?,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
        })
    }

    /// Drains this session's output into its emulator. Returns whether
    /// anything changed and, once the client is gone, its exit code.
    fn poll(&mut self) -> (bool, Option<i32>) {
        let mut changed = false;
        let mut exited = None;
        while let Ok(event) = self.child.events.try_recv() {
            changed = true;
            match event {
                ChildEvent::Output(bytes) => self.parser.process(&bytes),
                ChildEvent::Exited(code) => exited = Some(code),
                // A crewmate's decision is answered by the firstmate running
                // that crewmate, not from this pane, so the sentinel scrolling
                // past here is ordinary output and nothing more.
                ChildEvent::Decision(_) | ChildEvent::DecisionParseError(_) => {}
            }
        }
        (changed, exited)
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows, cols) == self.parser.screen().size() {
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        // A crewmate's session that will not take a resize still draws, just
        // at the wrong width, which is not worth an alert over the pane.
        let _ = self.child.resize(rows, cols);
    }

    /// Ends this client. A tmux client leaving is a detach, so the crewmate's
    /// own session keeps running with its harness untouched.
    fn close(&mut self) {
        self.child.kill();
    }
}

/// How a crewmate's session is launched. `podman_crew_session` is the real
/// one; the tests point this at an ordinary child so the whole look-in and
/// attach path runs over a real pty without a live podman.
type CrewLauncher = fn(&Crewmate, Join) -> (String, Vec<String>);

fn podman_crew_session(crew: &Crewmate, join: Join) -> (String, Vec<String>) {
    crew::session_command(&crew.name, join)
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
    /// Whether the selected task's full description is popped over the TUI.
    /// Raised by walking the backlog in command mode, dropped as soon as the
    /// captain navigates away from it.
    task_detail: bool,
    /// Whether the last drawn layout actually seated the tasks pane; a
    /// terminal too narrow for it must not pop a detail overlay for a pane
    /// that is not on screen.
    tasks_visible: bool,
    /// The same for the crew pane: a selection the captain cannot see is not
    /// one to preview or attach to.
    crew_visible: bool,
    /// The live look-in on the selected crewmate, raised by walking the crew
    /// in command mode and dropped as soon as the captain leaves it.
    crew_preview: Option<CrewSession>,
    /// The crewmate session the captain attached to, which owns the agent pane
    /// until they return to firstmate.
    attached: Option<CrewSession>,
    /// Size of the preview overlay's inside, kept by `sync_size` so a session
    /// opened between frames starts at roughly the right size.
    preview_size: (u16, u16),
    crew_launcher: CrewLauncher,
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
            task_detail: false,
            tasks_visible: false,
            crew_visible: false,
            crew_preview: None,
            attached: None,
            preview_size: INITIAL_SIZE,
            crew_launcher: podman_crew_session,
        }
    }

    /// Raises the live look-in on the crewmate under the cursor, replacing a
    /// preview of a different crewmate and leaving an unchanged one alone so a
    /// repeated key does not restart the client.
    fn show_crew_preview(&mut self) {
        let Some(crew) = self.crew.selected().filter(|_| self.crew_visible).cloned() else {
            self.close_crew_preview();
            return;
        };
        if self
            .crew_preview
            .as_ref()
            .is_some_and(|session| session.crew == crew.task)
        {
            return;
        }
        self.close_crew_preview();
        self.crew_preview = self.open_crew_session(&crew, Join::Preview, self.preview_size);
    }

    fn close_crew_preview(&mut self) {
        if let Some(mut session) = self.crew_preview.take() {
            session.close();
        }
    }

    /// Hands the agent pane to the crewmate under the cursor. Returns whether
    /// the captain is now looking at that crewmate's session.
    fn attach_to_selected_crew(&mut self) -> bool {
        let Some(crew) = self.crew.selected().filter(|_| self.crew_visible).cloned() else {
            return false;
        };
        self.close_crew_preview();
        let Some(session) = self.open_crew_session(&crew, Join::Attach, self.pty_size) else {
            return false;
        };
        self.detach();
        self.attached = Some(session);
        true
    }

    fn open_crew_session(
        &mut self,
        crew: &Crewmate,
        join: Join,
        size: (u16, u16),
    ) -> Option<CrewSession> {
        let (program, args) = (self.crew_launcher)(crew, join);
        match CrewSession::open(&crew.task, &program, &args, size.0, size.1) {
            Ok(session) => Some(session),
            Err(err) => {
                self.notice = Some(format!("could not reach {}: {err}", crew.task));
                None
            }
        }
    }

    fn detach(&mut self) {
        if let Some(mut session) = self.attached.take() {
            session.close();
        }
    }

    /// Firstmate's own session back in the pane, with every overlay raised on
    /// the way out to a crewmate taken down with it.
    fn return_to_firstmate(&mut self) {
        self.detach();
        self.close_crew_preview();
        self.task_detail = false;
        self.focus = Focus::Terminal;
    }

    /// Replaces the backlog and keeps the overlay honest: a shrinking backlog
    /// can leave the cursor on nothing, and an overlay with no task behind it
    /// would linger invisibly.
    fn set_tasks(&mut self, tasks: Vec<tasks::Task>) -> bool {
        let changed = self.tasks.set(tasks);
        if self.tasks.selected().is_none() {
            self.task_detail = false;
        }
        changed
    }

    fn start_harness(&mut self, root: &std::path::Path, harness: Harness) {
        if let Err(err) = config::save_default_harness(root, harness) {
            self.notice = Some(format!("could not save the default harness: {err}"));
        }
        self.harness = Some(harness);
        let (rows, cols) = self.pty_size;
        let args: Vec<String> = harness.args().iter().map(|a| a.to_string()).collect();
        match child::spawn(harness.command(), &args, Some(root), rows, cols) {
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

    /// Everything the TUI started, so no pty client outlives it.
    fn kill_child(&mut self) {
        self.close_crew_preview();
        self.detach();
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
        let mut changed = !events.is_empty();
        for event in events {
            match event {
                ChildEvent::Output(bytes) => {
                    self.head.saw_output(HEAD_ECHO);
                    self.parser.process(&bytes);
                }
                ChildEvent::Decision(decision) => {
                    self.head.set_state(HeadState::Thinking);
                    // The other way the session comes to rest on the captain,
                    // and it arrives exactly once per decision.
                    ping::ping();
                    self.decision = Some(DecisionBox::new(decision));
                    // The overlay owns the keyboard, so a half-entered
                    // prefix chord and anything command mode popped over the
                    // TUI must not survive into it.
                    self.focus = Focus::Terminal;
                    self.task_detail = false;
                    self.close_crew_preview();
                }
                ChildEvent::DecisionParseError(err) => {
                    self.notice = Some(format!("malformed decision payload: {err}"));
                }
                ChildEvent::Exited(code) => {
                    self.exited = Some(code);
                    self.head.set_state(HeadState::Gone);
                }
            }
        }
        changed |= self.poll_crew_sessions();
        changed
    }

    /// The same for the crewmate sessions, whose clients ending is a session
    /// to take down rather than one to keep drawing. A crewmate's session
    /// closing under the captain puts them back at firstmate rather than
    /// leaving the pane frozen on a dead client.
    fn poll_crew_sessions(&mut self) -> bool {
        let mut changed = false;

        let preview_ended = match &mut self.crew_preview {
            Some(session) => {
                let (drew, exited) = session.poll();
                changed |= drew;
                exited.map(|code| (session.crew.clone(), code))
            }
            None => None,
        };
        if let Some((crew, code)) = preview_ended {
            self.close_crew_preview();
            if code != 0 {
                self.notice = Some(format!("could not look in on {crew} (exit {code})"));
            }
        }

        let attach_ended = match &mut self.attached {
            Some(session) => {
                let (drew, exited) = session.poll();
                changed |= drew;
                exited.map(|_| session.crew.clone())
            }
            None => None,
        };
        if let Some(crew) = attach_ended {
            self.detach();
            self.notice = Some(format!("{crew}'s session closed - back at firstmate"));
        }

        changed
    }

    /// Keeps the pty and the emulator the same size as the pane on screen,
    /// so the harness lays itself out to what the captain actually sees.
    fn sync_size(&mut self, screen: Rect) {
        let areas = compute_layout(screen);
        self.tasks_visible = areas.tasks.is_some();
        if !self.tasks_visible {
            self.task_detail = false;
        }
        self.crew_visible = areas.crew.is_some();
        if !self.crew_visible {
            self.close_crew_preview();
        }
        let pane = areas.pane;

        let preview = Block::default()
            .borders(Borders::ALL)
            .inner(crew_preview_rect(pane));
        let preview_size = (preview.height, preview.width);
        if preview_size.0 > 0 && preview_size.1 > 0 && preview_size != self.preview_size {
            self.preview_size = preview_size;
            if let Some(session) = &mut self.crew_preview {
                session.resize(preview_size.0, preview_size.1);
            }
        }

        let inner = Block::default().borders(Borders::ALL).inner(pane);
        let size = (inner.height, inner.width);
        if size.0 == 0 || size.1 == 0 || size == self.pty_size {
            return;
        }
        self.pty_size = size;
        self.parser.screen_mut().set_size(size.0, size.1);
        if let Some(session) = &mut self.attached {
            session.resize(size.0, size.1);
        }
        if let Some(child) = &mut self.child {
            if let Err(err) = child.resize(size.0, size.1) {
                self.notice = Some(format!("could not resize the harness: {err}"));
            }
        }
    }

    /// The emulator the captain is actually typing at, which is the crewmate's
    /// while they are attached and firstmate's own otherwise.
    fn screen(&self) -> &vt100::Screen {
        match &self.attached {
            Some(session) => session.parser.screen(),
            None => self.parser.screen(),
        }
    }

    fn send_key(&mut self, key: KeyEvent) {
        let modes = keys::Modes::application_cursor(self.screen().application_cursor());
        let Some(bytes) = keys::encode(key, modes) else {
            return;
        };
        // While the captain is attached the keys are the crewmate's, and the
        // figurehead follows firstmate's own session, so this must not tell it
        // whose turn it is over there.
        if let Some(session) = &mut self.attached {
            if let Err(err) = session.child.write_input(&bytes) {
                self.notice = Some(format!("could not reach {}: {err}", session.crew));
            }
            return;
        }
        // The harness echoes these bytes straight back as output, which is
        // indistinguishable from work it is doing; telling the head whose turn
        // it is now is what stops a pause in the captain's typing being read as
        // the harness handing the keyboard back.
        if is_submit(key) {
            self.head.captain_submitted();
        } else {
            self.head.captain_typed();
        }
        if let Some(Err(err)) = self.child.as_mut().map(|c| c.write_input(&bytes)) {
            self.notice = Some(format!("could not reach the harness: {err}"));
        }
    }

    /// True once there is nothing left to type into, which is what makes
    /// the plain quit keys safe to reclaim. A crewmate's session in the pane
    /// is still something to type into, whatever became of firstmate's own.
    fn pane_is_dead(&self) -> bool {
        self.attached.is_none() && (self.child.is_none() || self.exited.is_some())
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
    // Without the disambiguating keyboard protocol a terminal reports
    // Shift+Enter as a plain Enter, so the harness can never be told to open a
    // new line instead of submitting. Terminals that do not support it are left
    // alone rather than sent a sequence they would print.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(err.into());
        }
    };
    if enhanced {
        let _ = execute!(
            terminal.backend_mut(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }

    let result = run(&mut terminal, &mut app, &root);
    app.kill_child();

    // A failed restore step must not strand the captain in raw mode on the
    // alternate screen, so every step runs regardless of the previous one.
    restore_terminal(&mut terminal, enhanced);

    result
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, enhanced: bool) {
    if enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

fn show_first_run_loading_screen() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut screen = loading::LoadingScreen::new("welcome aboard, captain - setting up the first slice");
    let start = Instant::now();
    let mut result = Ok(());
    while start.elapsed() < Duration::from_millis(900) {
        screen.progress = ((start.elapsed().as_millis() * 100) / 900).min(100) as u16;
        if let Err(err) = terminal.draw(|frame| screen.render(frame, frame.area())) {
            result = Err(err.into());
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    restore_terminal(&mut terminal, false);
    result
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
    app.set_tasks(tasks::load(root));
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
            if app.set_tasks(tasks::load(root)) {
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

        // A lull always brings the ship to rest, but only the end of a harness
        // turn is the captain's turn arriving, and it is reported once, so the
        // ping neither repeats while the session sits idle nor answers the
        // captain's own typing.
        match app.head.settle(HEAD_QUIET, app.decision.is_some()) {
            Settled::Unchanged => {}
            Settled::Quiet => dirty = true,
            Settled::YourTurn => {
                dirty = true;
                ping::ping();
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
            // The way home, from wherever command mode can be reached from:
            // firstmate's own session back in the pane with every overlay down.
            if key.code == KeyCode::Char('f') {
                app.return_to_firstmate();
                return Step::Continue;
            }
            // Esc is the deliberate way out of command mode, and it takes any
            // overlay command mode raised down with it. It leaves a crewmate
            // the captain attached to in the pane, since that is the session
            // they asked for; `f` is what comes back from it.
            if key.code == KeyCode::Esc {
                app.close_crew_preview();
                app.focus = Focus::Terminal;
                app.task_detail = false;
                return Step::Continue;
            }
            // Walking the sidebars keeps command mode so a run of keys walks
            // the lists; the status bar names these while command mode is up.
            // Up/Down walk the backlog and Left/Right walk the crew, one axis
            // per sidebar, and each takes the other's overlay down so only one
            // is ever up. Walking the backlog pops the selected task's full
            // description over the TUI, since the sidebar can only show a
            // clipped title; walking the crew pops a live look-in on the
            // selected crewmate's own session.
            match key.code {
                KeyCode::Up => {
                    return command_stay(app, |a| {
                        a.tasks.scroll_up();
                        a.close_crew_preview();
                        a.task_detail = a.tasks_visible;
                    })
                }
                KeyCode::Down => {
                    return command_stay(app, |a| {
                        a.tasks.scroll_down();
                        a.close_crew_preview();
                        a.task_detail = a.tasks_visible;
                    })
                }
                KeyCode::Left => {
                    return command_stay(app, |a| {
                        a.crew.select_prev();
                        a.task_detail = false;
                        a.show_crew_preview();
                    })
                }
                KeyCode::Right => {
                    return command_stay(app, |a| {
                        a.crew.select_next();
                        a.task_detail = false;
                        a.show_crew_preview();
                    })
                }
                // Enter on a crewmate hands them the pane. With no crewmate
                // under the cursor there is nothing to open, so it falls
                // through and just leaves command mode.
                KeyCode::Enter => {
                    if app.attach_to_selected_crew() {
                        app.focus = Focus::Terminal;
                        return Step::Continue;
                    }
                }
                _ => {}
            }
            // Ctrl+B twice is how a literal Ctrl+B gets through to the
            // harness; anything else just leaves command mode.
            if is_prefix(key) {
                app.send_key(key);
            }
            app.focus = Focus::Terminal;
            app.task_detail = false;
            app.close_crew_preview();
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

/// The figurehead's eight art lines plus its label, inside its border.
const HEAD_HEIGHT: u16 = 11;
const SIDEBAR_WIDTH: u16 = 24;
/// Wide enough to seat the figurehead unclipped inside its border.
const RIGHT_WIDTH: u16 = head::FIGUREHEAD_WIDTH + 2;
const MIN_PANE_WIDTH: u16 = 30;

/// The full running layout: one full-height row of tasks sidebar | agent pane |
/// right column, with the figurehead above the crew in that right column, and a
/// single status line under the lot. The agent pane stays the centerpiece; on a
/// terminal too narrow to seat both columns and a usable pane, they collapse
/// and the pane spans the whole width rather than corrupting the layout.
struct Areas {
    head: Option<Rect>,
    tasks: Option<Rect>,
    pane: Rect,
    crew: Option<Rect>,
    status: Rect,
}

fn compute_layout(area: Rect) -> Areas {
    let [main, status] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .areas(area);

    if area.width >= SIDEBAR_WIDTH + MIN_PANE_WIDTH + RIGHT_WIDTH {
        let [tasks, pane, right] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(SIDEBAR_WIDTH),
                Constraint::Min(MIN_PANE_WIDTH),
                Constraint::Length(RIGHT_WIDTH),
            ])
            .areas(main);
        // `Max` on the figurehead so a short terminal shrinks it rather than
        // squeezing the crew list out of the column entirely.
        let [head, crew] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Max(HEAD_HEIGHT), Constraint::Min(3)])
            .areas(right);
        Areas {
            head: Some(head),
            tasks: Some(tasks),
            pane,
            crew: Some(crew),
            status,
        }
    } else {
        Areas {
            head: None,
            tasks: None,
            pane: main,
            crew: None,
            status,
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
    let areas = compute_layout(frame.area());

    if let Some(head_area) = areas.head {
        app.head.render(frame, head_area);
    }
    if let Some(tasks_area) = areas.tasks {
        app.tasks.render(frame, tasks_area);
    }
    if let Some(crew_area) = areas.crew {
        app.crew.render(frame, crew_area);
    }

    // The pane is firstmate's own session unless the captain attached to a
    // crewmate, in which case it is theirs; firstmate's keeps running behind
    // it either way.
    let screen = app.screen();
    let title = match &app.attached {
        Some(session) => format!(" {} ", session.crew),
        None => match app.harness {
            Some(harness) => format!(" {harness} "),
            None => " agent ".to_string(),
        },
    };
    // The captain's caret belongs to the harness, so it is hidden while
    // anything else owns the keyboard.
    let cursor_visible =
        !screen.hide_cursor() && app.decision.is_none() && app.focus == Focus::Terminal;
    let pane = PseudoTerminal::new(screen)
        .block(Block::default().borders(Borders::ALL).title(title))
        .cursor(Cursor::default().visibility(cursor_visible));
    frame.render_widget(pane, areas.pane);

    let (hint, hint_style, legend_allowed) = status_hint(app);
    let mut status_area = areas.status;
    if legend_allowed {
        // The legend only takes room the hint is not using, so a narrow
        // terminal keeps the whole bar for the keys.
        let budget = status_area
            .width
            .saturating_sub(hint.chars().count() as u16 + 1);
        if let Some(legend) = legend_line(budget) {
            let width = legend.width() as u16;
            let legend_area = Rect {
                x: status_area.x + status_area.width - width,
                width,
                ..status_area
            };
            status_area.width -= width;
            frame.render_widget(Paragraph::new(legend), legend_area);
        }
    }
    frame.render_widget(
        Paragraph::new(Line::from(hint)).style(hint_style),
        status_area,
    );

    // The detail overlay is up only while the captain is walking the backlog,
    // and a decision box outranks it.
    if app.task_detail && app.decision.is_none() {
        if let Some(task) = app.tasks.selected() {
            tasks::render_detail(frame, centered_rect(70, 60, frame.area()), task);
        }
    }

    // The look-in is up only while the captain is walking the crew, and a
    // decision box outranks it the same way. It is the crewmate's real screen,
    // rendered from a read-only client, so the caret is not the captain's to
    // show.
    if app.decision.is_none() {
        if let Some(session) = &app.crew_preview {
            let popup = crew_preview_rect(areas.pane);
            frame.render_widget(Clear, popup);
            let title = format!(" {} - looking in ", session.crew);
            let look = PseudoTerminal::new(session.parser.screen())
                .block(Block::default().borders(Borders::ALL).title(title))
                .cursor(Cursor::default().visibility(false));
            frame.render_widget(look, popup);
        }
    }

    if let Some(decision) = &app.decision {
        let popup = centered_rect(60, 40, areas.pane);
        frame.render_widget(Clear, popup);
        decision.render(frame, popup);
    }
}

/// The one always-visible reminder of who owns the keyboard and how to get
/// out, since Ctrl+C now belongs to the harness. The third field says whether
/// the legend may share the bar: an alert keeps the whole width.
fn status_hint(app: &App) -> (String, Style, bool) {
    let (text, style) = if let Some(notice) = &app.notice {
        (
            format!(" {notice} "),
            Style::default().fg(Color::Black).bg(Color::Red),
        )
    } else if app.decision.is_some() {
        (
            " decision - up/down to choose, enter to pick, esc to dismiss ".to_string(),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    } else if let Some(session) = &app.attached {
        // Whatever became of firstmate's own session behind it, what the pane
        // is showing is the crewmate's, so that is what the bar reports.
        (
            format!(
                " attached to {} - ctrl+b then f returns to firstmate ",
                session.crew
            ),
            Style::default().fg(Color::Black).bg(Color::Magenta),
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
    } else {
        match app.focus {
            Focus::Command => (
                " command - q quits, up/down tasks, left/right crew, enter attaches, ctrl+b then f firstmate, esc returns ".to_string(),
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

    let legend_allowed = app.notice.is_none()
        && app.exited.is_none()
        && app.child.is_some()
        && app.decision.is_none();
    (text, style, legend_allowed)
}

/// One legend chunk: a pane heading, or a swatch and what it means. The
/// meanings are the ones `tasks::task_item` and `crew::crew_item` paint, so a
/// change there has to be mirrored here.
struct LegendChunk {
    spans: Vec<Span<'static>>,
    heading: bool,
}

fn swatch(glyph: &'static str, color: Color, label: &'static str) -> LegendChunk {
    LegendChunk {
        spans: vec![
            Span::styled(glyph, Style::default().fg(color)),
            Span::styled(format!(" {label}  "), Style::default().fg(Color::DarkGray)),
        ],
        heading: false,
    }
}

fn heading(text: &'static str) -> LegendChunk {
    LegendChunk {
        spans: vec![Span::styled(
            text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )],
        heading: true,
    }
}

/// The legend, trimmed to `budget` columns by dropping whole entries off the
/// end - and any pane heading left with nothing under it - so a narrow
/// terminal loses meanings rather than overflowing the bar.
fn legend_line(budget: u16) -> Option<Line<'static>> {
    let mut chunks = vec![
        heading("tasks "),
        swatch(">", Color::Cyan, "in flight"),
        swatch("-", Color::Gray, "queued"),
        swatch("x", Color::DarkGray, "done"),
        swatch("[hold]", Color::DarkGray, "held/blocked"),
        heading("crew "),
        swatch("+", Color::Green, "working"),
        swatch("!", Color::Yellow, "stalled"),
        swatch("x", Color::Red, "stopped"),
        swatch("?", Color::DarkGray, "unknown"),
        LegendChunk {
            spans: vec![
                Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
                Span::styled(" selected", Style::default().fg(Color::DarkGray)),
            ],
            heading: false,
        },
    ];

    let width = |chunks: &[LegendChunk]| -> usize {
        chunks
            .iter()
            .flat_map(|c| c.spans.iter())
            .map(|s| s.content.chars().count())
            .sum()
    };
    while !chunks.is_empty()
        && (width(&chunks) > budget as usize || chunks.last().is_some_and(|c| c.heading))
    {
        chunks.pop();
    }

    if chunks.is_empty() {
        return None;
    }
    Some(Line::from(
        chunks.into_iter().flat_map(|c| c.spans).collect::<Vec<_>>(),
    ))
}

/// Where the live look-in on a crewmate sits: centred over the agent pane, so
/// firstmate's own session stays visible around it. `sync_size` and the draw
/// both compute it from the same pane rect, which is what keeps the emulator
/// the same size as the overlay it is drawn into.
fn crew_preview_rect(pane: Rect) -> Rect {
    centered_rect(70, 60, pane)
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
        app.tasks_visible = true;
        app.crew_visible = true;
        app
    }

    /// `cat` on a pty is a live session that echoes, which is enough to prove
    /// which session the pane is showing and where the captain's keys land.
    fn fake_crew_session(_: &Crewmate, _: Join) -> (String, Vec<String>) {
        ("cat".to_string(), Vec::new())
    }

    /// A running app whose crewmate sessions are those local children.
    fn app_with_crew() -> App {
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        app.crew_launcher = fake_crew_session;
        app.crew.set(sample_crew());
        app
    }

    fn pump_until(app: &mut App, done: impl Fn(&App) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            app.poll_child();
            if done(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
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
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

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
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());
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
                None,
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
        let pane = compute_layout(Rect::new(0, 0, 100, 40)).pane;

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

        let pane = compute_layout(screen).pane;
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
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

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

    /// The legend explains the panes' colours, and a narrow bar has to lose
    /// meanings from the end rather than overflow - never leaving a pane
    /// heading with nothing under it.
    #[test]
    fn the_legend_trims_to_the_width_it_is_given() {
        let full = legend_line(200).expect("a wide bar shows the whole legend");
        let text: String = full.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("in flight") && text.contains("stopped"),
            "{text}"
        );
        assert!(full.width() <= 200);

        for budget in 0..=80u16 {
            match legend_line(budget) {
                Some(line) => {
                    assert!(line.width() <= budget as usize, "overflowed at {budget}");
                    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    assert!(
                        !text.trim_end().ends_with("crew") && !text.trim_end().ends_with("tasks"),
                        "dangling heading at {budget}: {text:?}"
                    );
                }
                None => assert!(budget < 20, "gave up too early at {budget}"),
            }
        }
    }

    /// End to end over a real pty: harness output is a byte stream the
    /// emulator renders, and the decision sentinel still has to be spotted
    /// in that same stream.
    #[test]
    fn harness_output_reaches_both_the_emulator_and_the_decision_scanner() {
        let mut app = running_app();
        let mut child = child::spawn("cat", &[], None, 24, 80).unwrap();
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

    /// Every region the captain expects has to be on screen at once: the
    /// figurehead and the crew stacked in the top-right column, the tasks
    /// sidebar, and their live data, all around the agent pane.
    #[test]
    fn the_full_layout_shows_every_region() {
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        app.tasks
            .set(tasks::parse_backlog("## In flight\n- [ ] tui-layout - build it (since 2026-07-27)\n"));
        app.crew.set(
            crew::parse_ps(
                r#"[{"Names":["fm-h-tui-layout"],"State":"running","Status":"Up 2 minutes","Labels":{"firstmate.task":"tui-layout"}}]"#,
            )
            .expect("sample podman ps json parses"),
        );

        let rendered = render_to_string(&app, 100, 40);

        assert!(rendered.contains("tasks"), "tasks sidebar title missing");
        assert!(rendered.contains("crew"), "crew sidebar title missing");
        assert!(rendered.contains("firstmate"), "figurehead title missing");
        assert!(rendered.contains("tui-layout"), "task id missing");
    }

    /// The context and model readouts are already inside the agent pane, so
    /// the bottom bar must not repeat them.
    #[test]
    fn the_bottom_bar_no_longer_repeats_the_model_or_context() {
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        let rendered = render_to_string(&app, 100, 40);
        assert!(!rendered.contains("model:"), "model chip should be gone");
        assert!(!rendered.contains("context"), "context readout should be gone");
    }

    /// The spec's shape: tasks and the agent pane run the full height of the
    /// main area from its very top, with the figurehead above the crew in the
    /// top-right column and only the status line below them.
    #[test]
    fn the_panes_fill_the_main_area_with_the_head_above_the_crew() {
        let screen = Rect::new(0, 0, 100, 40);
        let areas = compute_layout(screen);
        let tasks = areas.tasks.expect("tasks pane");
        let head = areas.head.expect("figurehead");
        let crew = areas.crew.expect("crew pane");

        assert_eq!((tasks.y, areas.pane.y, head.y), (0, 0, 0), "all start at the top");
        assert_eq!(tasks.height, areas.pane.height, "tasks fills the pane's height");
        assert_eq!(tasks.bottom(), areas.status.y, "only the status line is below");

        assert_eq!(head.x, crew.x, "the head and the crew share the right column");
        assert_eq!(head.bottom(), crew.y, "the crew sits directly under the head");
        assert_eq!(crew.bottom(), areas.status.y);
        assert!(
            head.width >= head::FIGUREHEAD_WIDTH + 2,
            "the figurehead must fit inside its border"
        );
        assert_eq!(areas.status.height, 1);
    }

    /// A short terminal must keep a usable crew list rather than letting the
    /// figurehead take the whole column.
    #[test]
    fn a_short_terminal_shrinks_the_head_before_the_crew() {
        let areas = compute_layout(Rect::new(0, 0, 100, 12));
        let head = areas.head.expect("figurehead");
        let crew = areas.crew.expect("crew pane");
        assert!(crew.height >= 3, "the crew keeps a usable minimum");
        assert!(head.height < HEAD_HEIGHT, "the head gives way first");
        assert_eq!(head.bottom(), crew.y);
    }

    /// The figurehead has to show the live session state, not a fixed pose.
    #[test]
    fn the_figurehead_follows_the_live_session_state() {
        let mut app = running_app();
        assert!(render_to_string(&app, 100, 40).contains("idling"));

        app.head.set_state(HeadState::Sailing);
        assert!(render_to_string(&app, 100, 40).contains("sailing"));

        app.exited = Some(0);
        app.head.set_state(HeadState::Gone);
        assert!(render_to_string(&app, 100, 40).contains("off watch"));
    }

    /// The captain typing and then pausing rang the ping at them: the harness
    /// echoes their keystrokes back, which put the ship under sail, and the
    /// pause that followed looked exactly like a turn ending. Only a turn the
    /// captain actually handed over may ring.
    #[test]
    fn the_captains_own_typing_never_arms_the_ping() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

        for c in "ahoy".chars() {
            handle_running_key(&mut app, key(KeyCode::Char(c)));
        }
        // What the harness echoes back is those same keystrokes.
        app.head.set_state(HeadState::Sailing);
        assert_eq!(
            app.head.settle(Duration::ZERO, false),
            Settled::Quiet,
            "a pause mid-sentence is not the captain's turn"
        );

        // Submitting hands the turn over, so the lull at the end of the work
        // that follows is the real one.
        handle_running_key(&mut app, key(KeyCode::Enter));
        app.head.set_state(HeadState::Sailing);
        assert_eq!(app.head.settle(Duration::ZERO, false), Settled::YourTurn);

        app.kill_child();
    }

    /// The other half of the same echo. Those keystrokes coming back also
    /// looked exactly like the harness producing, which put an idle session
    /// under full sail while the captain was only composing.
    #[test]
    fn the_captains_own_typing_never_puts_the_ship_under_sail() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

        for c in "ahoy".chars() {
            handle_running_key(&mut app, key(KeyCode::Char(c)));
        }
        // What the harness writes on the heels of those keystrokes is those
        // same keystrokes.
        app.head.saw_output(HEAD_ECHO);
        let rendered = render_to_string(&app, 100, 40);
        app.kill_child();

        assert!(
            rendered.contains("idling") && !rendered.contains("sailing"),
            "expected the ship still at anchor, got {rendered:?}"
        );
    }

    /// Shift+Enter opens a new line in the harness's input rather than
    /// submitting, so the captain is still mid-sentence.
    #[test]
    fn shift_enter_does_not_hand_the_turn_over() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

        handle_running_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        );
        app.head.set_state(HeadState::Sailing);
        assert_eq!(app.head.settle(Duration::ZERO, false), Settled::Quiet);

        app.kill_child();
    }

    /// Walking the backlog pops the whole item over the TUI, because the
    /// sidebar can only ever show a clipped title.
    #[test]
    fn walking_the_backlog_pops_the_task_detail_and_dismisses_it_on_the_way_out() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());
        app.tasks.set(tasks::parse_backlog(
            "## In flight\n- [ ] tui-layout - build it (since 2026-07-27)\n  the whole story of the task\n",
        ));

        handle_running_key(&mut app, ctrl(PREFIX));
        assert!(!app.task_detail, "the chord alone must not pop the overlay");

        handle_running_key(&mut app, key(KeyCode::Down));
        assert!(app.task_detail);
        let rendered = render_to_string(&app, 100, 40);
        assert!(
            rendered.contains("the whole story of the task"),
            "expected the full description, got {rendered:?}"
        );

        // Navigating to the crew list, and leaving command mode entirely, both
        // take the overlay back down.
        handle_running_key(&mut app, key(KeyCode::Right));
        assert!(!app.task_detail);

        handle_running_key(&mut app, key(KeyCode::Up));
        assert!(app.task_detail);
        handle_running_key(&mut app, key(KeyCode::Esc));
        assert!(!app.task_detail);
        assert!(!render_to_string(&app, 100, 40).contains("the whole story"));

        app.kill_child();
    }

    /// A terminal too narrow for both columns and a usable pane must collapse
    /// them and keep the pane, not panic or corrupt the layout.
    #[test]
    fn a_narrow_terminal_collapses_the_sidebars() {
        let wide = compute_layout(Rect::new(0, 0, 100, 40));
        assert!(wide.tasks.is_some() && wide.crew.is_some());

        let narrow = compute_layout(Rect::new(0, 0, 50, 20));
        assert!(narrow.tasks.is_none() && narrow.crew.is_none() && narrow.head.is_none());
        // The pane keeps the whole middle width when the sidebars are gone.
        assert_eq!(narrow.pane.width, 50);

        // Rendering at a squeeze must not panic.
        let mut app = running_app();
        app.harness = Some(Harness::Claude);
        let _ = render_to_string(&app, 50, 20);
        let _ = render_to_string(&app, 8, 6);
    }

    /// A collapsed tasks pane has nothing for the detail overlay to belong to,
    /// so walking the backlog must not pop it over the agent pane.
    #[test]
    fn a_narrow_terminal_keeps_the_task_detail_down() {
        let mut app = running_app();
        app.set_tasks(tasks::parse_backlog(
            "## In flight\n- [ ] tui-layout - build it (since 2026-07-27)\n  the whole story\n",
        ));
        app.sync_size(Rect::new(0, 0, 50, 20));

        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Down));
        assert!(!app.task_detail);
        assert!(!render_to_string(&app, 50, 20).contains("the whole story"));
    }

    /// A backlog that shrinks out from under the cursor must take the overlay
    /// with it rather than leaving it raised over nothing.
    #[test]
    fn an_emptied_backlog_drops_the_task_detail() {
        let mut app = running_app();
        app.set_tasks(tasks::parse_backlog(
            "## In flight\n- [ ] tui-layout - build it (since 2026-07-27)\n  the whole story\n",
        ));
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Down));
        assert!(app.task_detail);

        app.set_tasks(Vec::new());
        assert!(!app.task_detail);
    }

    fn sample_crew() -> Vec<crew::Crewmate> {
        crew::parse_ps(
            r#"[
              {"Names":["fm-ab12-one"],"State":"running","Status":"Up 2 minutes","Labels":{"firstmate.task":"one"}},
              {"Names":["fm-ab12-two"],"State":"running","Status":"Up 3 minutes","Labels":{"firstmate.task":"two"}}
            ]"#,
        )
        .expect("sample podman ps json parses")
    }

    /// Left/Right are the crew's own axis, the mirror of Up/Down on the
    /// backlog, and they keep command mode so a run of them walks the roster.
    #[test]
    fn command_mode_left_and_right_walk_the_crew() {
        let mut app = app_with_crew();

        handle_running_key(&mut app, ctrl(PREFIX));
        assert_eq!(
            handle_running_key(&mut app, key(KeyCode::Right)),
            Step::Continue
        );
        assert_eq!(app.focus, Focus::Command, "a crew key keeps command mode");
        assert_eq!(app.crew.selected().map(|c| c.task.as_str()), Some("two"));

        handle_running_key(&mut app, key(KeyCode::Right));
        assert_eq!(
            app.crew.selected().map(|c| c.task.as_str()),
            Some("two"),
            "the cursor stops at the end of the roster"
        );

        handle_running_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.crew.selected().map(|c| c.task.as_str()), Some("one"));
        handle_running_key(&mut app, key(KeyCode::Left));
        assert_eq!(
            app.crew.selected().map(|c| c.task.as_str()),
            Some("one"),
            "the cursor stops at the start of the roster"
        );

        app.kill_child();
    }

    /// An empty roster has nothing to pick, and walking it must not panic or
    /// invent a selection.
    #[test]
    fn walking_an_empty_crew_selects_nothing() {
        let mut app = running_app();
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        handle_running_key(&mut app, key(KeyCode::Left));
        assert!(app.crew.selected().is_none());
        assert_eq!(app.focus, Focus::Command);
    }

    /// Esc is the deliberate way out of command mode from any point in it,
    /// and it must not reach the harness as a keystroke either.
    #[test]
    fn esc_leaves_command_mode_from_anywhere_in_it() {
        let mut app = app_with_crew();

        // Straight out of the bare chord.
        handle_running_key(&mut app, ctrl(PREFIX));
        assert_eq!(handle_running_key(&mut app, key(KeyCode::Esc)), Step::Continue);
        assert_eq!(app.focus, Focus::Terminal);

        // And out of a walk of either sidebar.
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        handle_running_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Terminal);
        assert!(app.crew_preview.is_none(), "esc takes the look-in down too");

        app.kill_child();
    }

    /// Walking the crew is the crew's own overlay: a live look-in on whichever
    /// crewmate is under the cursor, which follows it and gives way to the
    /// backlog's overlay rather than stacking with it.
    #[test]
    fn walking_the_crew_raises_a_look_in_that_tracks_the_selection() {
        let mut app = app_with_crew();

        handle_running_key(&mut app, ctrl(PREFIX));
        assert!(
            app.crew_preview.is_none(),
            "the chord alone must not raise it"
        );

        handle_running_key(&mut app, key(KeyCode::Right));
        assert_eq!(
            app.crew_preview.as_ref().map(|s| s.crew.as_str()),
            Some("two")
        );

        handle_running_key(&mut app, key(KeyCode::Left));
        assert_eq!(
            app.crew_preview.as_ref().map(|s| s.crew.as_str()),
            Some("one"),
            "the look-in follows the cursor"
        );

        // Walking back to the backlog takes it down and pops that overlay.
        handle_running_key(&mut app, key(KeyCode::Down));
        assert!(app.crew_preview.is_none());
        assert!(app.task_detail);

        app.kill_child();
    }

    /// The look-in is the crewmate's real screen, not a summary of it.
    #[test]
    fn the_look_in_renders_the_crewmates_own_screen() {
        let mut app = app_with_crew();
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));

        app.crew_preview
            .as_mut()
            .expect("a look-in on the selected crewmate")
            .child
            .write_input(b"ahoy from the crew\r")
            .unwrap();
        let arrived = pump_until(&mut app, |a| {
            a.crew_preview
                .as_ref()
                .is_some_and(|s| s.parser.screen().contents().contains("ahoy from the crew"))
        });
        assert!(arrived, "the crewmate's own output never reached the look-in");

        let rendered = render_to_string(&app, 100, 40);
        app.kill_child();
        assert!(
            rendered.contains("ahoy from the crew"),
            "expected the crewmate's screen in the look-in, got {rendered:?}"
        );
        assert!(rendered.contains("looking in"), "{rendered:?}");
    }

    /// Enter hands the pane to the crewmate. Firstmate's own session has to
    /// keep running behind it, which is what makes the way back instant.
    #[test]
    fn enter_attaches_to_the_crewmate_and_leaves_firstmate_running() {
        let mut app = app_with_crew();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        assert_eq!(handle_running_key(&mut app, key(KeyCode::Enter)), Step::Continue);

        assert_eq!(app.attached.as_ref().map(|s| s.crew.as_str()), Some("two"));
        assert_eq!(app.focus, Focus::Terminal, "the crewmate gets the keyboard");
        assert!(app.crew_preview.is_none(), "the look-in gives way to the pane");
        assert!(app.child.is_some(), "firstmate's own session keeps running");

        // The captain's keys now land in the crewmate's session, not firstmate's.
        for c in "ahoy".chars() {
            handle_running_key(&mut app, key(KeyCode::Char(c)));
        }
        let landed = pump_until(&mut app, |a| {
            a.attached
                .as_ref()
                .is_some_and(|s| s.parser.screen().contents().contains("ahoy"))
        });
        let rendered = render_to_string(&app, 100, 40);
        let firstmate_screen = app.parser.screen().contents();
        app.kill_child();

        assert!(landed, "the captain's keys never reached the crewmate");
        assert!(
            !firstmate_screen.contains("ahoy"),
            "firstmate's own session must not have been typed at, got {firstmate_screen:?}"
        );
        assert!(rendered.contains("attached to two"), "{rendered:?}");
    }

    /// Esc is only a way out of command mode, so it must not undo the boarding
    /// the captain asked for; `f` is what comes back from that.
    #[test]
    fn esc_drops_the_look_in_but_leaves_the_captain_attached() {
        let mut app = app_with_crew();
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        handle_running_key(&mut app, key(KeyCode::Enter));

        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Left));
        assert!(app.crew_preview.is_some());

        handle_running_key(&mut app, key(KeyCode::Esc));
        assert!(app.crew_preview.is_none());
        assert_eq!(app.focus, Focus::Terminal);
        assert_eq!(
            app.attached.as_ref().map(|s| s.crew.as_str()),
            Some("two"),
            "esc must not put the captain back at firstmate"
        );

        app.kill_child();
    }

    /// The one key that always gets home, from a look-in, from an attached
    /// crewmate, and from a backlog overlay alike.
    #[test]
    fn the_chord_then_f_returns_to_firstmate_from_anywhere() {
        let mut app = app_with_crew();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

        // From an attached crewmate, with a look-in raised over it as well.
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        handle_running_key(&mut app, key(KeyCode::Enter));
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Left));
        assert!(app.attached.is_some() && app.crew_preview.is_some());

        // Walking the crew is already command mode, so `f` lands directly,
        // exactly as `q` does.
        assert_eq!(handle_running_key(&mut app, key(KeyCode::Char('f'))), Step::Continue);
        assert!(app.attached.is_none() && app.crew_preview.is_none());
        assert_eq!(app.focus, Focus::Terminal);
        let rendered = render_to_string(&app, 100, 40);
        assert!(
            rendered.contains("claude") && !rendered.contains("attached to"),
            "expected firstmate's own session back in the pane, got {rendered:?}"
        );

        // And from a backlog overlay, which is the other thing command mode
        // can leave standing.
        app.set_tasks(tasks::parse_backlog(
            "## In flight\n- [ ] tui-layout - build it (since 2026-07-27)\n  the whole story\n",
        ));
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Down));
        assert!(app.task_detail);
        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Char('f')));
        assert!(!app.task_detail);
        assert_eq!(app.focus, Focus::Terminal);

        app.kill_child();
    }

    /// A crewmate's session ending under the captain - they detached inside
    /// tmux, or it died - has to put them back at firstmate rather than
    /// leaving the pane frozen on a dead client.
    #[test]
    fn a_crewmate_session_closing_puts_the_captain_back_at_firstmate() {
        let mut app = app_with_crew();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());

        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        handle_running_key(&mut app, key(KeyCode::Enter));
        app.attached.as_mut().expect("an attached crewmate").close();

        let returned = pump_until(&mut app, |a| a.attached.is_none());
        let notice = app.notice.clone();
        app.kill_child();

        assert!(returned, "the closed session was never taken down");
        assert!(
            notice.is_some_and(|n| n.contains("two")),
            "the captain should be told whose session closed"
        );
    }

    /// A collapsed crew pane has no visible selection, so there is nothing to
    /// look in on and nothing to attach to either.
    #[test]
    fn a_narrow_terminal_keeps_the_look_in_down_and_attaches_to_nothing() {
        let mut app = app_with_crew();
        app.sync_size(Rect::new(0, 0, 50, 20));

        handle_running_key(&mut app, ctrl(PREFIX));
        handle_running_key(&mut app, key(KeyCode::Right));
        assert!(app.crew_preview.is_none());

        assert_eq!(handle_running_key(&mut app, key(KeyCode::Enter)), Step::Continue);
        assert!(app.attached.is_none());

        app.kill_child();
    }

    /// Command mode walks the sidebars in place and only leaves for a key
    /// that is not a navigation key.
    #[test]
    fn command_mode_scrolls_the_sidebars_and_stays() {
        let mut app = running_app();
        app.child = Some(child::spawn("cat", &[], None, 24, 80).unwrap());
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
