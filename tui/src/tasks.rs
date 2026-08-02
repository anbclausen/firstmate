//! The left `tasks` sidebar: a scrollable list built by reading firstmate's
//! backlog markdown (`data/backlog.md`) directly, with no agent involvement.
//!
//! The parse is a plain, tolerant read of the markdown `tasks-axi` writes; that
//! tool's grammar is the single owner of the format, and this only needs the
//! id, a clean title, which section the item sits in, and whether it is held or
//! blocked. File I/O is kept out of `parse_backlog` so the parsing is testable
//! without a real backlog on disk.

use std::fs;
use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    InFlight,
    Queued,
    Done,
}

impl TaskState {
    /// Sort key: in-flight work is the most relevant, done is history.
    fn rank(self) -> u8 {
        match self {
            TaskState::InFlight => 0,
            TaskState::Queued => 1,
            TaskState::Done => 2,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            TaskState::InFlight => ">",
            TaskState::Queued => "-",
            TaskState::Done => "x",
        }
    }

    fn color(self) -> Color {
        match self {
            TaskState::InFlight => Color::Cyan,
            TaskState::Queued => Color::Gray,
            TaskState::Done => Color::DarkGray,
        }
    }

    fn label(self) -> &'static str {
        match self {
            TaskState::InFlight => "in flight",
            TaskState::Queued => "queued",
            TaskState::Done => "done",
        }
    }

    /// The section header state, matching `tasks-axi`'s markdown backend: a
    /// column-0 `## In flight` / `## Queued` / `## Done...` heading.
    fn from_header(text: &str) -> Option<TaskState> {
        let t = text.trim().to_lowercase();
        if t == "in flight" {
            Some(TaskState::InFlight)
        } else if t == "queued" {
            Some(TaskState::Queued)
        } else if t.starts_with("done") {
            Some(TaskState::Done)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub state: TaskState,
    pub held: bool,
    pub blocked: bool,
    /// The item's full prose: the bullet head exactly as written, tags and all,
    /// followed by its indented body lines. The sidebar shows the cleaned-up
    /// title; the detail overlay shows this.
    pub detail: String,
}

/// The backlog path, resolved off the repo root the same robust way
/// `config::config_path` resolves the harness file (`AGENTS.md` section 2 pins
/// the backlog to `data/backlog.md`).
pub fn backlog_path(repo_root: &Path) -> PathBuf {
    repo_root.join("data").join("backlog.md")
}

/// Reads and parses the backlog, returning an empty list when the file is
/// absent or unreadable so the sidebar degrades to "no tasks" rather than
/// failing the whole TUI.
pub fn load(repo_root: &Path) -> Vec<Task> {
    fs::read_to_string(backlog_path(repo_root))
        .map(|src| parse_backlog(&src))
        .unwrap_or_default()
}

/// Parse the backlog markdown into a flat, relevance-ordered task list. Any
/// preamble is ignored; a bullet head in a recognized section becomes a `Task`,
/// and the indented body lines that follow it become that task's detail.
pub fn parse_backlog(src: &str) -> Vec<Task> {
    let mut tasks: Vec<Task> = Vec::new();
    let mut section: Option<TaskState> = None;
    // The bullet a body line may attach to, cleared at every section header so
    // an indented line can never land on a task from the section above it.
    let mut open: Option<usize> = None;

    for raw in src.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);

        // A column-0 `##` followed by whitespace opens a section (an indented
        // `  ## ...` body heading never does, matching the tasks-axi grammar).
        if line.starts_with("##") && line[2..].starts_with(char::is_whitespace) {
            section = TaskState::from_header(&line[2..]);
            open = None;
            continue;
        }

        let Some(state) = section else { continue };
        if let Some((id, rest)) = match_bullet(line, state) {
            let (title, held, blocked) = clean_title(rest);
            open = Some(tasks.len());
            tasks.push(Task {
                id: id.to_string(),
                title,
                state,
                held,
                blocked,
                detail: rest.trim().to_string(),
            });
        } else if line.starts_with("  ") && !line.trim().is_empty() {
            // A body continuation line belongs to the bullet above it.
            if let Some(task) = open.and_then(|i| tasks.get_mut(i)) {
                task.detail.push('\n');
                task.detail.push_str(line.trim());
            }
        }
    }

    // Stable sort keeps file order within a section while grouping the sections
    // in relevance order even if the file ever lists them out of order.
    tasks.sort_by_key(|t| t.state.rank());
    tasks
}

/// The section header carries the state, not the checkbox mark, so `- [ ]` and
/// `- [x]` are both accepted everywhere (matching `bin/fm-backlog-handoff.sh`'s
/// `^- \[[ x]\] +`). Older tasks-axi output used `- **<id>**` for in-flight
/// items, so that form is recognized there too.
fn match_bullet(line: &str, state: TaskState) -> Option<(&str, &str)> {
    match state {
        TaskState::InFlight => parse_star(line).or_else(|| parse_checkbox(line)),
        _ => parse_checkbox(line),
    }
}

fn parse_checkbox(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("- [")?;
    let rest = rest
        .strip_prefix(' ')
        .or_else(|| rest.strip_prefix('x'))?
        .strip_prefix(']')?;
    let trimmed = rest.trim_start_matches(' ');
    if trimmed.len() == rest.len() {
        return None;
    }
    split_id_rest(trimmed)
}

fn parse_star(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("- **")?;
    let end = rest.find("** - ")?;
    let id = &rest[..end];
    if valid_id(id) {
        Some((id, &rest[end + "** - ".len()..]))
    } else {
        None
    }
}

/// `<id> - <rest>`: the id has no spaces, so the first ` - ` ends it.
fn split_id_rest(rest: &str) -> Option<(&str, &str)> {
    let sep = rest.find(" - ")?;
    let id = &rest[..sep];
    if valid_id(id) {
        Some((id, &rest[sep + 3..]))
    } else {
        None
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Strip the trailing canonical tag region off a bullet's prose to recover a
/// clean display title, and report whether the item is held or blocked. This is
/// a display-grade cleanup, not the authoritative round-tripping parse that
/// tasks-axi's own grammar owns.
fn clean_title(rest: &str) -> (String, bool, bool) {
    let held = rest.contains("(hold:");
    let mut blocked = false;

    let mut s: String = rest.trim().to_string();
    loop {
        let trimmed = s.trim_end();
        if let Some(stripped) = strip_trailing_paren_tag(trimmed) {
            s = stripped;
            continue;
        }
        if let Some(stripped) = strip_trailing_dep(trimmed) {
            s = stripped;
            blocked = true;
            continue;
        }
        break;
    }
    (s.trim().to_string(), held, blocked)
}

const DEP_MARKERS: [&str; 3] = ["blocked-by:", "parent:", "discovered-from:"];

/// Byte offset where the trailing dependency region starts, if the tail of `s`
/// is one: a run of `<marker> <id>` pairs reaching the end of the line, the last
/// of which may carry the canonical ` - <reason>` tail. Prose that merely
/// contains a marker word (`Fix parent: field handling`) is not a dependency
/// region, so it stays in the title and never flags the task blocked.
fn dep_region_start(s: &str) -> Option<usize> {
    let tokens = tokens_with_offsets(s);
    (0..tokens.len())
        .find(|&i| is_dep_region(&tokens[i..]))
        .map(|i| tokens[i].0)
}

fn tokens_with_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(b) = start.take() {
                tokens.push((b, &s[b..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(b) = start {
        tokens.push((b, &s[b..]));
    }
    tokens
}

fn is_dep_region(tokens: &[(usize, &str)]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i].1;
        let Some(marker) = DEP_MARKERS.iter().find(|m| token.starts_with(**m)) else {
            return false;
        };
        let inline = &token[marker.len()..];
        i += 1;
        if inline.is_empty() {
            match tokens.get(i) {
                Some((_, id)) if valid_id(id) => i += 1,
                _ => return false,
            }
        } else if !valid_id(inline) {
            return false;
        }
        // `<marker> <id> - <reason>`: the reason runs to end of line.
        if matches!(tokens.get(i), Some((_, "-"))) {
            return true;
        }
    }
    true
}

fn strip_trailing_dep(s: &str) -> Option<String> {
    let at = dep_region_start(s)?;
    Some(s[..at].trim_end().to_string())
}

fn strip_trailing_paren_tag(s: &str) -> Option<String> {
    let s = s.trim_end();
    if !s.ends_with(')') {
        return None;
    }
    let open = s.rfind('(')?;
    let content = &s[open + 1..s.len() - 1];
    if content.contains('(') || content.contains(')') {
        return None;
    }
    if is_tag_content(content) {
        Some(s[..open].trim_end().to_string())
    } else {
        None
    }
}

/// Recognize the trailing parenthetical tags tasks-axi emits, so an ordinary
/// mid-title parenthetical is left in the prose.
fn is_tag_content(content: &str) -> bool {
    let c = content.trim();
    let lower = c.to_lowercase();
    const KEYS: [&str; 6] = [
        "kind:",
        "priority:",
        "hold:",
        "hold-kind:",
        "hold-until:",
        "repo:",
    ];
    if KEYS.iter().any(|k| lower.starts_with(k)) {
        return true;
    }
    // `(since <date>)` and the `(merged|reported|done|closed <date>)` closures.
    let mut words = c.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(verb), Some(date), None) => {
            matches!(
                verb.to_lowercase().as_str(),
                "since" | "merged" | "reported" | "done" | "closed"
            ) && is_date(date)
        }
        _ => false,
    }
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 10
        && b.iter().enumerate().all(|(i, &c)| {
            if i == 4 || i == 7 {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// The left sidebar's view state: the parsed tasks plus a scroll/highlight
/// cursor. `set` clamps the cursor so a shrinking backlog never leaves it out
/// of range.
pub struct TasksPanel {
    pub tasks: Vec<Task>,
    state: ListState,
}

impl TasksPanel {
    pub fn new() -> Self {
        TasksPanel {
            tasks: Vec::new(),
            state: ListState::default(),
        }
    }

    /// Replace the task list, keeping the cursor in range. Returns whether the
    /// list actually changed, so the caller can avoid a needless redraw.
    pub fn set(&mut self, tasks: Vec<Task>) -> bool {
        if tasks == self.tasks {
            return false;
        }
        self.tasks = tasks;
        self.clamp();
        true
    }

    fn clamp(&mut self) {
        if self.tasks.is_empty() {
            self.state.select(None);
        } else {
            let last = self.tasks.len() - 1;
            let selected = self.state.selected().unwrap_or(0).min(last);
            self.state.select(Some(selected));
        }
    }

    /// The task the cursor is on, which is what the detail overlay shows.
    pub fn selected(&self) -> Option<&Task> {
        self.state.selected().and_then(|i| self.tasks.get(i))
    }

    pub fn scroll_down(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let last = self.tasks.len() - 1;
        let next = self.state.selected().map(|s| (s + 1).min(last)).unwrap_or(0);
        self.state.select(Some(next));
    }

    pub fn scroll_up(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let prev = self.state.selected().map(|s| s.saturating_sub(1)).unwrap_or(0);
        self.state.select(Some(prev));
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::ALL).title("tasks");

        if self.tasks.is_empty() {
            let empty = List::new(vec![ListItem::new(Line::from(Span::styled(
                "no tasks",
                Style::default().fg(Color::DarkGray),
            )))])
            .block(block);
            frame.render_widget(empty, area);
            return;
        }

        let items: Vec<ListItem> = self.tasks.iter().map(task_item).collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("");

        let mut state = self.state.clone();
        frame.render_stateful_widget(list, area, &mut state);
    }
}

impl Default for TasksPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the overlay's content is taller than the room inside its borders.
/// Pure, so the cue is testable without a terminal.
fn detail_overflows(header: &str, detail: &str, area: Rect) -> bool {
    let (Some(width), Some(height)) = (area.width.checked_sub(2), area.height.checked_sub(2)) else {
        return true;
    };
    let rows = std::iter::once(header)
        .chain(std::iter::once(""))
        .chain(detail.lines())
        .map(|line| wrapped_rows(line, width))
        .sum::<usize>();
    rows > usize::from(height)
}

/// Rows one line takes once word-wrapped to `width`, mirroring the `Wrap`
/// the paragraph is rendered with.
fn wrapped_rows(line: &str, width: u16) -> usize {
    let width = usize::from(width).max(1);
    let mut rows = 1;
    let mut used = 0;
    for word in line.split(' ') {
        let len = word.chars().count().max(1);
        if used == 0 {
            used = len;
        } else if used + 1 + len <= width {
            used += 1 + len;
        } else {
            rows += 1;
            used = len;
        }
        while used > width {
            rows += 1;
            used -= width;
        }
    }
    rows
}

/// The detail overlay: the selected task's full description, popped over the
/// TUI while the captain walks the backlog. The sidebar can only show a
/// truncated title, so this is where the whole item is legible.
pub fn render_detail(frame: &mut Frame, area: Rect, task: &Task) {
    let header = format!("{}  [{}]", task.id, task.state.label());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            task.id.clone(),
            Style::default()
                .fg(task.state.color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  [{}]", task.state.label()),
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    lines.push(Line::from(""));
    lines.extend(
        task.detail
            .lines()
            .map(|line| Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Gray)))),
    );

    // The overlay is a fixed size, so a long item is simply cut off; say so
    // rather than letting the captain read a silently truncated description.
    let title = if detail_overflows(&header, &task.detail, area) {
        "task - truncated"
    } else {
        "task"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn task_item(task: &Task) -> ListItem<'static> {
    let mut title_style = Style::default().fg(Color::Gray);
    let mut suffix = "";
    if task.blocked {
        title_style = Style::default().fg(Color::DarkGray);
        suffix = " [blocked]";
    } else if task.held {
        title_style = Style::default().fg(Color::DarkGray);
        suffix = " [hold]";
    }

    let line = Line::from(vec![
        Span::styled(
            format!("{} ", task.state.glyph()),
            Style::default().fg(task.state.color()),
        ),
        Span::styled(
            task.id.clone(),
            Style::default()
                .fg(task.state.color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}{}", task.title, suffix), title_style),
    ]);
    ListItem::new(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Backlog

## In flight
- [ ] sm-thing - secondmate charter (kind: secondmate) (since 2026-07-27)
- [ ] tui-layout - Build the full firstmate-tui layout (since 2026-07-27)
  Intent line one
  second line
## Queued
- [ ] crew-health - Wire crew sidebar to podman ps (since 2026-07-27)
- [ ] held-thing - Waiting on captain (since 2026-07-27) (hold: captain decision pending) (hold-kind: captain)
- [ ] blocked-thing - Depends on tui-layout blocked-by: tui-layout (since 2026-07-27)
## Done
- [x] old-thing - A finished thing (done 2026-07-27)
";

    #[test]
    fn parses_every_section_and_orders_by_relevance() {
        let tasks = parse_backlog(SAMPLE);
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "sm-thing",
                "tui-layout",
                "crew-health",
                "held-thing",
                "blocked-thing",
                "old-thing",
            ]
        );
        assert_eq!(tasks[0].state, TaskState::InFlight);
        assert_eq!(tasks[2].state, TaskState::Queued);
        assert_eq!(tasks[5].state, TaskState::Done);
    }

    #[test]
    fn strips_trailing_tags_from_titles() {
        let tasks = parse_backlog(SAMPLE);
        assert_eq!(tasks[0].title, "secondmate charter");
        assert_eq!(tasks[1].title, "Build the full firstmate-tui layout");
        assert_eq!(tasks[5].title, "A finished thing");
    }

    #[test]
    fn flags_held_and_blocked_items() {
        let tasks = parse_backlog(SAMPLE);
        let held = tasks.iter().find(|t| t.id == "held-thing").unwrap();
        assert!(held.held);
        assert_eq!(held.title, "Waiting on captain");

        let blocked = tasks.iter().find(|t| t.id == "blocked-thing").unwrap();
        assert!(blocked.blocked);
        assert_eq!(blocked.title, "Depends on tui-layout");
    }

    #[test]
    fn ignores_body_continuation_lines() {
        let tasks = parse_backlog(SAMPLE);
        assert!(tasks.iter().all(|t| t.id != "Intent"));
        assert_eq!(tasks.len(), 6);
    }

    /// The sidebar shows a cleaned-up title, so the whole item - tags and body
    /// alike - has to survive somewhere for the detail overlay to show.
    #[test]
    fn keeps_the_whole_item_as_the_detail() {
        let tasks = parse_backlog(SAMPLE);
        let task = tasks.iter().find(|t| t.id == "tui-layout").unwrap();
        assert_eq!(
            task.detail,
            "Build the full firstmate-tui layout (since 2026-07-27)\nIntent line one\nsecond line"
        );
        // A body belongs to its own bullet, not the next one.
        assert_eq!(
            tasks.iter().find(|t| t.id == "sm-thing").unwrap().detail,
            "secondmate charter (kind: secondmate) (since 2026-07-27)"
        );
    }

    /// A body line can only ever belong to a bullet from its own section, so
    /// stray indented text under a new heading must not land on the last task.
    #[test]
    fn a_body_line_never_crosses_a_section_header() {
        let src = "## In flight\n- [ ] a - x\n## Queued\n  stray indented note\n- [ ] b - y\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks.iter().find(|t| t.id == "a").unwrap().detail, "x");
        assert_eq!(tasks.iter().find(|t| t.id == "b").unwrap().detail, "y");
    }

    fn detail_task(detail: &str) -> Task {
        Task {
            id: "t".to_string(),
            title: "t".to_string(),
            state: TaskState::Queued,
            held: false,
            blocked: false,
            detail: detail.to_string(),
        }
    }

    /// The overlay is a fixed size, so the captain needs to be told when the
    /// description it shows is not all of it.
    #[test]
    fn the_overlay_flags_only_content_that_does_not_fit() {
        let area = Rect::new(0, 0, 20, 6);
        let short = detail_task("one\ntwo");
        let header = format!("{}  [{}]", short.id, short.state.label());
        assert!(!detail_overflows(&header, &short.detail, area));

        let long = detail_task(&"line\n".repeat(20));
        assert!(detail_overflows(&header, &long.detail, area));

        // Wrapping counts too: one long line can outgrow the box on its own.
        let wrapped = detail_task(&"word ".repeat(40));
        assert!(detail_overflows(&header, &wrapped.detail, area));
    }

    #[test]
    fn recognizes_the_legacy_in_flight_bullet() {
        let src = "## In flight\n- **old-style** - a legacy in-flight bullet (since 2026-07-27)\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "old-style");
        assert_eq!(tasks[0].state, TaskState::InFlight);
        assert_eq!(tasks[0].title, "a legacy in-flight bullet");
    }

    #[test]
    fn the_checkbox_mark_and_spacing_do_not_gate_the_section() {
        let src = "\
## In flight
- [x] marked-in-flight - still in flight (since 2026-07-27)
## Queued
- [ ]  double-space - extra spacing (since 2026-07-27)
## Done
- [ ] unmarked-done - left unmarked (done 2026-07-27)
";
        let tasks = parse_backlog(src);
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["marked-in-flight", "double-space", "unmarked-done"]);
        assert_eq!(tasks[0].state, TaskState::InFlight);
        assert_eq!(tasks[1].state, TaskState::Queued);
        assert_eq!(tasks[1].title, "extra spacing");
        assert_eq!(tasks[2].state, TaskState::Done);
    }

    #[test]
    fn a_prose_parenthetical_mentioning_repo_survives() {
        let src = "## Queued\n- [ ] doc-fix - fix the docs (see repo: notes)\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks[0].title, "fix the docs (see repo: notes)");
    }

    #[test]
    fn prose_mentioning_a_dependency_word_survives() {
        let src = "## Queued\n- [ ] fix-parent - Fix parent: field handling in tasks-axi (since 2026-07-27)\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks[0].title, "Fix parent: field handling in tasks-axi");
        assert!(!tasks[0].blocked);
    }

    #[test]
    fn a_dependency_with_a_reason_strips_and_flags_blocked() {
        let src = "## Queued\n- [ ] legal-release - Release approval blocked-by: external-legal - external legal dependency (repo: sample) (kind: ship)\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks[0].title, "Release approval");
        assert!(tasks[0].blocked);
    }

    #[test]
    fn several_trailing_dependency_markers_strip_together() {
        let src = "## Queued\n- [ ] child - do the thing parent: tui-layout blocked-by: crew-health\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks[0].title, "do the thing");
        assert!(tasks[0].blocked);
    }

    #[test]
    fn a_mid_title_parenthetical_survives() {
        let src =
            "## Queued\n- [ ] doc-fix - fix report.md (reported earlier) wording (since 2026-07-27)\n";
        let tasks = parse_backlog(src);
        assert_eq!(tasks[0].title, "fix report.md (reported earlier) wording");
    }

    #[test]
    fn empty_or_headerless_input_yields_nothing() {
        assert!(parse_backlog("").is_empty());
        assert!(parse_backlog("just some prose\nwith no sections\n").is_empty());
    }

    #[test]
    fn set_clamps_the_cursor_when_the_backlog_shrinks() {
        let mut panel = TasksPanel::new();
        panel.set(parse_backlog(SAMPLE));
        for _ in 0..10 {
            panel.scroll_down();
        }
        assert_eq!(panel.state.selected(), Some(5));

        panel.set(parse_backlog("## In flight\n- [ ] only-one - solo (since 2026-07-27)\n"));
        assert_eq!(panel.state.selected(), Some(0));
    }

    #[test]
    fn set_reports_no_change_for_identical_input() {
        let mut panel = TasksPanel::new();
        assert!(panel.set(parse_backlog(SAMPLE)));
        assert!(!panel.set(parse_backlog(SAMPLE)));
    }
}
