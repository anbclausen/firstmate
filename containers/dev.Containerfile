# containers/dev.Containerfile - standard general dev/coding profile for the
# podman runtime backend (bin/backends/podman.sh), used whenever a spawned
# project has no Containerfile of its own at its repo root. See
# docs/podman-backend.md "Container profiles" for the selection contract and
# docs/configuration.md's "Runtime backend" section for the owning pointer.
#
# Scope: a plain, non-root shell environment with tmux (this adapter's own
# in-container event-source plumbing - see bin/backends/podman.sh's file
# header), git/gh, treehouse (for the worktree-isolation step every ship/
# scout spawn requires), no-mistakes (for the no-mistakes delivery mode),
# and Claude Code as the one baked-in harness for now (other harnesses
# remain a captain decision; see docs/podman-backend.md "Open questions").
FROM debian:12-slim
LABEL firstmate.managed=true

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
     tmux git ca-certificates curl openssh-client gnupg procps \
  && rm -rf /var/lib/apt/lists/*

# GitHub CLI (gh) - apt.github.com because Debian's own repo lags behind.
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
     -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
     > /etc/apt/sources.list.d/github-cli.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends gh \
  && rm -rf /var/lib/apt/lists/*

# Node.js, for Claude Code.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
  && apt-get install -y --no-install-recommends nodejs \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code

# treehouse and no-mistakes install as root (their installers shell out to
# sudo when not root, which isn't present in this slim image) so the
# binaries land in a system path shared by every user.
RUN curl -fsSL https://kunchenguid.github.io/treehouse/install.sh | sh
RUN curl -fsSL https://raw.githubusercontent.com/kunchenguid/no-mistakes/main/docs/install.sh | sh \
  && chmod o+x /root \
  && chmod -R o+rX /root/.no-mistakes

# The project bind mount is owned by whatever uid runs podman on the host,
# which almost never matches this image's "agent" uid, so git's ownership
# check refuses every operation ("detected dubious ownership") without this.
RUN git config --system --add safe.directory '*'

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /work

CMD ["sleep", "infinity"]
