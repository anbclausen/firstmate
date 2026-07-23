# firstmate TUI

A `ratatui` + `crossterm` terminal frontend that wraps a firstmate primary session, replacing the plain `claude`/`codex`/`opencode`/`pi`/`grok` terminal with a captain-facing view.

This is a first working slice.
It does not yet wire into firstmate's session-backend machinery (tmux, herdr, zellij, cmux, Orca); it runs a chosen harness as a standalone child process on a pty.

## Building and running

```
cargo build
cargo test
cargo run
```

On first launch, no default harness is chosen yet, so the TUI shows a picker (claude, codex, opencode, pi, grok - the verified harnesses from `AGENTS.md` section 4).
The choice is saved to `config/tui-harness`, local and gitignored like the rest of this repo's `config/*` files (`AGENTS.md` section 2).
Delete that file to be asked again.

## Layout

- An animated ASCII-art head region at the top, reflecting a simple state machine: idling, thinking, talking.
  Extend it by adding a new `HeadState` arm in `src/head.rs`.
- A transcript region below it, dimmed by default, streaming the wrapped harness's raw output.
  The captain is not meant to watch this closely; it exists for when input is needed.
- A decision box, rendered as a separate popup over the transcript rather than inline scrollback, whenever the wrapped harness emits a decision (see below).

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

A line that carries the sentinel but fails to parse as valid JSON is surfaced in the transcript as a malformed-decision notice rather than silently dropped.

## Loading screen

Shown on first launch, and (via the `FM_TUI_PODMAN_BUILD` environment variable, holding the program and arguments to run) whenever a podman image needs to be pulled or built first.
See `run.sh` and `firstmate.Containerfile` at the repo root for the container boot flow this is meant to eventually hook into.
A failed build/pull crashes the process and dumps the full captured log to stdout; it is never swallowed into a short summary.
