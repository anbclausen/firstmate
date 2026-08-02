# firstmate TUI

A `ratatui` + `crossterm` terminal frontend that wraps a firstmate primary session, replacing the plain `claude`/`codex`/`opencode`/`pi`/`grok` terminal with a captain-facing view.
The harness runs in an embedded terminal (`tui-term` over a `vt100` emulator), so it stays fully interactive rather than being reduced to a read-only transcript.

This is a first working slice.
It does not yet wire into firstmate's session-backend machinery (tmux, herdr, zellij, cmux, Orca); it runs a chosen harness as a standalone child process on a pty.

## Installing and running

Captain-facing setup is the root [`install.sh`](../install.sh) plus the installed `fm` command - see the root [`README.md`](../README.md) "Quick Start".
Podman is the only host prerequisite; `install.sh` compiles this crate inside `tui/Containerfile`'s build container (no host Rust toolchain needed), and the installed `fm` command itself relaunches into `tui/runtime.Containerfile`'s container before running (`src/container.rs`), so the TUI's real process gets the same podman-socket privileges the firstmate primary itself needs to see sibling crewmate containers.

For crate-local development instead:

```
cargo build
cargo test
cargo run
```

CI runs `cargo test` for this crate on every PR and merge, inside the same `tui/Containerfile` build image (the `TUI crate tests` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)).

`cargo run` on a bare host runs the same container relaunch `fm` does; set `FM_TUI_CONTAINERIZED=1` to skip it and run the TUI natively for a quick local iteration loop (no harness credential/socket mounts are set up in that mode).

On first launch, no default harness is chosen yet, so the TUI shows a picker (claude, codex, opencode, pi, grok - the verified harnesses from `AGENTS.md` section 4).
The choice is saved to `config/tui-harness`, local and gitignored like the rest of this repo's `config/*` files (`AGENTS.md` section 2).
Delete that file to be asked again.

## Layout

The screen is the agent pane framed by lightweight status regions; the pane is the centerpiece and is never dimmed.

- Left (`tasks`): a scrollable list of the backlog, read directly from `data/backlog.md` by `src/tasks.rs`.
  It is a plain programmatic parse of the markdown `tasks-axi` writes (no agent involvement), grouped most-relevant-first: in-flight, then queued, then done, colour-coded by section, with held and blocked items marked.
  The backlog path is resolved off the repo root the same way `src/config.rs` resolves the harness file, and the list refreshes on a timer.
- Centre: the agent pane, a real embedded terminal running the wrapped harness, not a rendered transcript.
  Harness output goes through a `vt100` emulator and is drawn by `tui-term` as a screen grid, so a full-screen harness renders normally instead of spilling escape codes.
  The pane's size drives the pty's size, so resizing the window re-lays-out the harness.
- Top right: the figurehead, an animated ASCII first mate at the helm showing the live session state - idling, thinking, talking, or off watch once the harness has exited.
  The state is driven by what the session actually does, and settles back to idling after a quiet spell rather than freezing on the last thing the harness did.
  A pending decision box holds the state instead, since the session really is blocked on it until the captain answers.
  The drawing is one frame with animated eye, mouth, and helm slots; extend it by adding a `HeadState` arm in `src/head.rs`.
- Under the figurehead (`crew`): a scrollable list of firstmate's crewmate containers and their health, read programmatically from `podman ps` by `src/crew.rs`.
  Crewmates are the containers carrying a `firstmate.task` label (see `bin/backends/podman.sh`); health (working, stalled, stopped) is derived from the container state podman reports.
  The `podman ps` read runs on a background thread and refreshes on a timer, so it never blocks the UI or forks podman per frame.
- Bottom: a one-line status bar which always shows who owns the keyboard and how to quit.
  The wrapped harness already shows its own model and context readouts inside the pane, so the bar does not repeat them.
- A task detail overlay, popped over the TUI while the captain walks the backlog (see below), and a decision box, rendered as a popup overlay on top of the agent pane whenever the wrapped harness emits a decision (see below).

The tasks pane, the agent pane, and the right-hand column all start at the very top of the screen and run its full height, with only the status line below them.
On a terminal too narrow to seat both columns and a usable pane, they collapse and the pane spans the whole width, so a small window stays usable rather than corrupting the layout.
A short terminal shrinks the figurehead before the crew list, so the crew always keeps a usable minimum.

## Focus and quitting

The agent pane is a real terminal, so it gets everything you type, `Ctrl+C` included - `Ctrl+C` interrupts the harness rather than the TUI.
That leaves no ordinary key free to quit on, so the TUI reserves exactly one chord for itself, tmux-style.

- `Ctrl+B` switches to command mode; the status bar turns cyan to say so.
- `Ctrl+B` then `q` quits the TUI and terminates the harness.
  This works whenever the harness owns the keyboard, which is every state except a decision box being up.
- `Ctrl+B` then `Up`/`Down` walks the `tasks` pane, and `Ctrl+B` then `PageUp`/`PageDown` scrolls the `crew` list; scroll keys keep command mode so a run of them walks the list.
  Walking the tasks pane pops the selected task's full description - the whole bullet plus its body lines - over the TUI, since the pane can only show a clipped title.
  It comes back down as soon as the captain scrolls the crew list, leaves command mode, or a decision box arrives, and it never comes up at all on a terminal too narrow to seat the tasks pane or once the backlog no longer has the selected task.
  The overlay is a fixed size, so an item too long to fit is titled `task - truncated` rather than being cut off silently.
- `Ctrl+B` then `Ctrl+B` sends a literal `Ctrl+B` on to the harness.
- `Ctrl+B` then any other key returns to the terminal without doing anything.
- Once the harness has exited there is nothing left to type into, so plain `q`, `Esc`, and `Ctrl+C` quit directly.

`src/keys.rs` owns the key-to-bytes translation, including the control bytes, the arrow and function-key escape sequences, and the DECCKM (application cursor keys) variants a full-screen harness switches on.
`Shift+Enter` sends a bare line feed so it opens a new line in the harness's input instead of submitting it; that needs the terminal to tell the two apart, so the TUI asks for the disambiguating keyboard protocol on startup where the terminal supports it.

## The decision protocol

This is the one wire format a wrapped agent uses to signal "this is a decision point" instead of scrolling past it as ordinary output.
`src/decision.rs` is this contract's only owner; nothing else in this repo restates it.

The wrapped process emits a single line of JSON on its own line, prefixed by a sentinel:

```
::firstmate-decision:: {"prompt": "merge now?", "options": ["yes", "no"]}
```

- `prompt` - the question shown in the decision box.
- `options` - the agent's own choices, in display order.

The TUI always appends two more choices after the agent's own list, never supplied by the agent: `Something else` and `Chat about this`.
Selecting either does not resolve the decision by itself; it is meant to hand control to a free-text reply channel instead of a fixed choice (that channel is not yet wired up in this slice).

`decision::Scanner` watches the same pty byte stream the emulator renders, reassembling lines across read boundaries, so the sentinel is still caught now that the pane is a terminal rather than a line feed.
While the decision box is up it is modal: it owns the keyboard, so `Up`/`Down` and `Enter` pick a choice instead of reaching the harness, and `Esc` dismisses the box and hands the keyboard back.
The chord is inert while the box is up, and a decision arriving mid-chord cancels it, so dismiss or answer the box first and then use `Ctrl+B` `q` to quit.

A line that carries the sentinel but fails to parse as valid JSON is surfaced in the status bar as a malformed-decision notice rather than silently dropped.

## Loading screen

Shown on first launch (inside the container, once a harness is picked), and, in the outer host relaunch phase, whenever the runtime image needs building first - `FM_TUI_PODMAN_BUILD` overrides that build command (see `src/container.rs`).
A failed build/pull crashes the process and dumps the full captured log to stdout; it is never swallowed into a short summary.
