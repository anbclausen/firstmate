<h1 align="center">firstmate</h1>

<h3 align="center">Talk to one agent. Ship with a crew.</h3>

firstmate itself, the whole workflow of one agent dispatching and supervising a crew of agents, is [Kun Chen](https://x.com/kunchenguid)'s project: [kunchenguid/firstmate](https://github.com/kunchenguid/firstmate).
It's a genuinely great piece of design - go use it.

This is [anbclausen](https://github.com/anbclausen)'s fork, and it adds exactly two things on top: containerizing everything for safety (firstmate and every crewmate it spawns run in podman, least-privilege, instead of directly on your host), and a TUI built specifically for the firstmate workflow, rather than a general-purpose one like herdr.

![firstmate TUI](assets/tui-placeholder.png)

## Requirements

Requires [podman](https://podman.io), running.

## Quick Start

```sh
git clone https://github.com/anbclausen/firstmate
cd firstmate
./install.sh
fm
```

`fm` opens the TUI.
First launch walks you through picking an agent and any login it needs, then you're talking to your first mate.

## Built-in skills

Firstmate ships these user-invocable built-in skills.
Claude and grok use the slash form shown here; codex uses the same names with `$`, such as `$afk`.

| Skill              | What it does                                                                                                                                  |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `/afk`             | Enter away-mode supervision: the sub-supervisor self-handles routine notifications, escalates captain-relevant events and bounded declared-external-wait rechecks as batched digests, and actively alerts if delivery gets stuck while you step away |
| `/ahoy`            | Recap what happened in the visible session since your last message, plus any decisions still waiting on you, falling back to Bearings when it is the session's first message |
| `/bearings`        | Generate a standalone current-status report from bounded local fleet and registered-secondmate state, with live PR enrichment only when requested, written to a dated file in `data/` and surfaced concisely in chat; read-mostly, mutates no task state |
| `/updatefirstmate` | Self-update the running firstmate and its secondmates to the latest from origin with fast-forward-only pulls, then re-read instructions and nudge secondmates |
| `/stow`            | Sweep the session for uncaptured durable knowledge, route each finding to its disk home per AGENTS.md, file undone next steps to the backlog, and report what is now safe to reset |

## Documentation

- [docs/architecture.md](docs/architecture.md) - how the crew, supervision, worktrees, and project modes work.
- [docs/configuration.md](docs/configuration.md) - environment variables, backends, and config files.
- [docs/wedge-alarm.md](docs/wedge-alarm.md) - away-mode wedge alarm setup and alert directives.
- Runtime backends - [docs/tmux-backend.md](docs/tmux-backend.md) (the verified reference), plus the experimental [docs/herdr-backend.md](docs/herdr-backend.md), [docs/zellij-backend.md](docs/zellij-backend.md), [docs/orca-backend.md](docs/orca-backend.md), and [docs/cmux-backend.md](docs/cmux-backend.md).
- [docs/documentation-audiences.md](docs/documentation-audiences.md) - which audience each documentation surface serves.
- [CONTRIBUTING.md](CONTRIBUTING.md) - how to contribute.

## License

MIT - see [LICENSE](LICENSE).
