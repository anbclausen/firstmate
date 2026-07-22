<h1 align="center">firstmate</h1>

<h3 align="center">Talk to one agent. Ship with a crew.</h3>

firstmate itself, the whole workflow of one agent dispatching and supervising a crew of agents, is [Kun Chen](https://x.com/kunchenguid)'s project: [kunchenguid/firstmate](https://github.com/kunchenguid/firstmate).
It's a genuinely great piece of design - go use it.

This is [anbclausen](https://github.com/anbclausen)'s fork, and it adds exactly two things on top: containerizing everything for safety (firstmate and every crewmate it spawns run in podman, least-privilege, instead of directly on your host), and a TUI built specifically for the firstmate workflow, rather than a general-purpose one like herdr.

![firstmate TUI](assets/tui-placeholder.png)

## Quick Start

Requires [podman](https://podman.io), running.

```sh
git clone https://github.com/anbclausen/firstmate
cd firstmate
./install.sh
fm
```

`fm` opens the TUI.
First launch walks you through picking an agent and any login it needs, then you're talking to your first mate.

## Documentation

- [docs/architecture.md](docs/architecture.md) - how the crew, supervision, worktrees, and project modes work.
- [docs/configuration.md](docs/configuration.md) - environment variables, backends, and config files.
- [CONTRIBUTING.md](CONTRIBUTING.md) - how to contribute.

## License

MIT - see [LICENSE](LICENSE).
