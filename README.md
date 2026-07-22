<h1 align="center">firstmate</h1>
<p align="center">
  <a
    href="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue?style=flat-square"
    ><img
      alt="Platform"
      src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue?style=flat-square"
  /></a>
</p>

<h3 align="center">Talk to one agent. Ship with a crew.</h3>

<p align="center">
  <img alt="firstmate - talk to one agent, ship with a crew" src="assets/banner.png" width="100%" />
</p>

## The original firstmate

This whole distro - the first-mate/crewmate model, the dispatch and zero-token supervision engine, disposable git worktrees, project delivery modes, secondmates, the harness and backend adapters (tmux, herdr, zellij, cmux, Orca) - is [Kun Chen](https://x.com/kunchenguid)'s design: **[kunchenguid/firstmate](https://github.com/kunchenguid/firstmate)**.
It's a genuinely well-built piece of engineering: a restart-proof, guarded-by-construction system for running a fleet of coding agents from one chat, with real thought put into safety (a captain-only merge authority, worktree isolation, fail-closed locks) and into making multi-agent work legible instead of a wall of terminal noise.
Full credit for the architecture and the original docs goes there - go read [its README](https://github.com/kunchenguid/firstmate) and [docs/architecture.md](docs/architecture.md) for the complete picture.

## This rendition

This is [anbclausen](https://github.com/anbclausen)'s fork of the original, built to make firstmate itself containerized end to end:

- **Podman crewmate backend** - crewmates run in least-privilege podman containers instead of directly on the host: `bin/backends/podman.sh`, the `podman-containerization` skill, and standard `containers/dev.Containerfile` / `containers/scout.Containerfile` profiles.
- **Containerized primary** - root `run.sh` + `firstmate.Containerfile` boot firstmate itself inside a podman container, not just the crewmates it spawns, mounting host Claude credentials and the podman-machine's own socket so the containerized primary can spawn sibling crewmate containers beside itself.
- **In progress**: a Rust (ratatui) TUI wrapping this workflow - see `data/backlog.md` for the current plan.

## Quick Start

### Requirements

- A verified agent harness: Claude Code, Grok, Pi, Codex, or OpenCode.
- Git and the GitHub CLI, authenticated through `gh auth login`.
- tmux, for the reference session backend.

The first mate detects and offers to install everything else.

### Recommended harnesses

**Claude Code, Grok, and Pi are equal co-primary recommendations** for running the primary firstmate session.
Claude Code and Grok use background-notify wake cycles; Pi uses its tracked primary watcher extension.
All three have verified turn-end guard paths when launched with their documented setup.
Pick whichever one matches your subscription and workflow.

Codex and OpenCode are also verified and supported as primary harnesses; Codex uses bounded foreground checkpoints, and OpenCode uses a TUI plugin, so both carry more harness-specific supervision tradeoffs than the three co-primaries.

### Install and launch

```sh
gh auth login
git clone https://github.com/anbclausen/firstmate
cd firstmate
```

Then launch one of the co-primary harnesses; AGENTS.md takes over from there:

**Claude Code**

```sh
claude
```

**Grok**

```sh
grok --trust
```

**Pi**

```sh
pi
```

For Grok, `--trust` is needed once per clone so project hooks and the turn-end guard load; `/hooks-trust` inside Grok works too.
For Pi, approve the project trust prompt once per clone on first launch so both tracked `.pi/extensions/*.ts` files auto-load.

### Talk to it

```sh
> ahoy! look at my github project xyz, then fix the flaky login test and add dark mode

# firstmate checks its toolchain (asking your consent before installing anything),
# clones the project under projects/, and spawns two crewmates in the active backend
# fm-fix-login-k3 and fm-dark-mode-p7.
# Minutes later:

  PR ready for review, captain: https://github.com/you/xyz/pull/42
  (fix flaky login test - risk: low - CI green)

> alright merge it
```

### More backends

Setup guides for tmux (the default) and every other supported backend (herdr, zellij, Orca, cmux) are linked in [Documentation](#documentation) below.

The original's README covers the full request/dispatch/supervision flow, the built-in skills (`/afk`, `/bearings`, `/updatefirstmate`, `/stow`), and the two-tier skill layout in detail - see [kunchenguid/firstmate](https://github.com/kunchenguid/firstmate) and [docs/architecture.md](docs/architecture.md).

## Documentation

- [docs/architecture.md](docs/architecture.md) - how the crew, supervision, worktrees, secondmates, and project modes work.
- [docs/configuration.md](docs/configuration.md) - environment variables, `FM_HOME`, runtime backend selection, optional X mode, the files you set, and harness support.
- [docs/wedge-alarm.md](docs/wedge-alarm.md) - configure the active alert for an away-mode escalation delivery that gets stuck.
- [docs/tmux-backend.md](docs/tmux-backend.md) - setup guide for the tmux reference backend: prerequisites, attaching, and watching crew windows.
- [docs/herdr-backend.md](docs/herdr-backend.md) - setup guide for the experimental herdr backend, plus its verification notes and known gaps.
- [docs/zellij-backend.md](docs/zellij-backend.md) - setup guide for the experimental zellij backend, plus its verification notes and known gaps.
- [docs/orca-backend.md](docs/orca-backend.md) - setup guide for the experimental Orca backend, plus its lifecycle notes and known gaps.
- [docs/cmux-backend.md](docs/cmux-backend.md) - setup guide for the experimental cmux backend, plus its verification notes and known gaps.
- [docs/codex-app-backend.md](docs/codex-app-backend.md) - Codex App backend boundary, evidence, and rollout contract.
- [docs/gitlab-merge-watch.md](docs/gitlab-merge-watch.md) - how the merge watch follows a GitLab merge request on any instance, and the evidence behind it.
- [docs/turnend-guard.md](docs/turnend-guard.md) - the primary session's structural "no turn ends blind" backstop: verified per-harness hook mechanisms, scoping, loop safety, and fail-open tradeoffs.
- [docs/supervision-protocols/](docs/supervision-protocols/) - rendered primary-harness watcher protocols for Claude, Codex, OpenCode, Pi, Grok, and unknown harness fallback.
- [docs/scripts.md](docs/scripts.md) - the `bin/` toolbelt reference.
- [`AGENTS.md`](AGENTS.md) - the distro's always-loaded operating contract and routing index for conditional procedures.
- [CONTRIBUTING.md](CONTRIBUTING.md) - how to contribute, including the dev/test commands.

## Contributing

Contributions are welcome - see [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow, repo conventions, and how to run the tests.

## License

MIT - see [LICENSE](LICENSE).
