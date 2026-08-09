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
| `/afk`             | Enter away-mode supervision: the sub-supervisor self-handles routine notifications in bash, escalates captain-relevant events and bounded declared-external-wait rechecks as batched digests, and actively alerts if delivery gets stuck while you step away |
| `/ahoy`            | Recap visible session events since the prior real captain message plus visibly unanswered captain decisions, falling back to Bearings when invoked as the session's first real captain message |
| `/bearings`        | Generate a concise four-section chat digest from bounded local fleet and registered-secondmate state; use `/bearings file` to also replace today's dated report in `data/`, and add `include PRs` when live PR enrichment is wanted |
| `/updatefirstmate` | Self-update the running firstmate and its secondmates to the latest from origin with fast-forward-only pulls, then re-read instructions and nudge secondmates |
| `/stow`            | Sweep the session for uncaptured durable knowledge, route each finding to its disk home per AGENTS.md, file undone next steps to the backlog, cascade the same sweep to every registered second mate against that home's own memory budget, and report what is now safe to reset |

Bearings invocation examples:

- `/bearings` returns the fresh four-section digest in chat only.
- `/bearings include PRs` keeps chat-only mode and opts into live PR enrichment.
- `/bearings file` replaces today's `data/status-report-<YYYY-MM-DD>.md` from scratch and links it from the four-section chat digest.
- `/bearings file include PRs` combines the dated report with live PR enrichment.

## Documentation

- [docs/architecture.md](docs/architecture.md) - maintainer architecture for the crew, supervision, worktrees, secondmates, and project modes.
- [docs/configuration.md](docs/configuration.md) - environment variables, `FM_HOME`, runtime backend selection, optional Relay and its X and Discord setup steps, the files you set, and harness support.
- [docs/remote-secondmates.md](docs/remote-secondmates.md) - current setup, routing, transfer, recovery, and safety behavior for whole-home remote second mates.
- [docs/calm.md](docs/calm.md) - current Pi `/calm` behavior and supported presentation limits.
- [docs/wedge-alarm.md](docs/wedge-alarm.md) - configure the active alert for an away-mode escalation delivery that gets stuck.
- [docs/tmux-backend.md](docs/tmux-backend.md) - current setup and limits for the tmux reference backend.
- [docs/herdr-backend.md](docs/herdr-backend.md) - current setup, safety boundaries, and limits for the experimental Herdr backend.
- [docs/zellij-backend.md](docs/zellij-backend.md) - current setup and limits for the experimental Zellij backend.
- [docs/orca-backend.md](docs/orca-backend.md) - current setup and limits for the experimental Orca backend.
- [docs/cmux-backend.md](docs/cmux-backend.md) - current setup, socket security, and limits for the experimental cmux backend.
- [docs/podman-backend.md](docs/podman-backend.md) - current setup, container profiles, and limits for this fork's containerized podman backend.
- [tui/README.md](tui/README.md) - the `fm` TUI: what it wraps today, its harness picker, and crate-local development.
- [docs/codex-app-backend.md](docs/codex-app-backend.md) - the current blocked Codex App backend boundary and rollout contract.
- [docs/verification/runtime-backends.md](docs/verification/runtime-backends.md) - active maintainer verification for runtime backend guarantees.
- [docs/gitlab-merge-watch.md](docs/gitlab-merge-watch.md) - maintainer verification for GitLab merge watching on arbitrary instances.
- [docs/turnend-guard.md](docs/turnend-guard.md) - the primary session's current "no turn ends blind" backstop, scope, loop safety, and compatibility limits.
- [docs/verification/supervision.md](docs/verification/supervision.md) - active maintainer verification for session-start, guard, continuity, and wedge integrations.
- [docs/supervision-protocols/](docs/supervision-protocols/) - rendered primary-harness watcher protocols for Claude, Codex, OpenCode, Pi and `pi-signed`, Grok, and unknown harness fallback.
- [docs/scripts.md](docs/scripts.md) - the `bin/` toolbelt reference.
- [docs/documentation-audiences.md](docs/documentation-audiences.md) - documentation audiences and the machine-checked placement boundary.
- [`AGENTS.md`](AGENTS.md) - the distro's always-loaded operating contract and routing index for conditional procedures.
- [CONTRIBUTING.md](CONTRIBUTING.md) - how to contribute, including the dev/test commands.

## Contributing

Contributions are welcome - see [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow, repo conventions, and how to run the tests.

## License

MIT - see [LICENSE](LICENSE).
