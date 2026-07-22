# containers/dev.Containerfile - standard general dev/coding profile for the
# podman runtime backend (bin/backends/podman.sh), used whenever a spawned
# project has no Containerfile of its own at its repo root. See
# docs/podman-backend.md "Container profiles" for the selection contract and
# docs/configuration.md's "Runtime backend" section for the owning pointer.
#
# Scope: a plain, non-root shell environment with tmux (this adapter's own
# in-container event-source plumbing - see bin/backends/podman.sh's file
# header) and git/treehouse's runtime prerequisites. It intentionally does
# NOT bake in a specific agent harness binary (claude/codex/opencode/pi/grok)
# - harness installation/auth is out of scope for Phase 1 and stays a captain
# decision; see docs/podman-backend.md "Open questions".
FROM debian:12-slim
LABEL firstmate.managed=true

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
     tmux git ca-certificates curl openssh-client \
  && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /work

CMD ["sleep", "infinity"]
