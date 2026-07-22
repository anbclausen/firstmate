# firstmate.Containerfile - temporary bootstrap image to run firstmate
# ITSELF (the primary) inside a podman container, not just spawned
# crewmates. Scaffolding for the "everything containerized" phase ahead
# of the TUI; delete alongside root run.sh once the TUI supersedes it.
#
# Installs the same tool surface bin/fm-bootstrap.sh checks for on the
# host: tmux/git/gh for fleet plumbing, treehouse for worktrees, the
# node-based axi tool family, and no-mistakes. Claude Code itself is
# installed from its npm package rather than the host's brew cask, since
# the container has no Homebrew.
FROM debian:12-slim
LABEL firstmate.managed=true

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
     tmux git ca-certificates curl openssh-client gnupg podman procps \
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

# Node.js 22.x - lavish-axi requires >=22.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
  && apt-get install -y --no-install-recommends nodejs \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g \
     @anthropic-ai/claude-code \
     gh-axi \
     chrome-devtools-axi \
     lavish-axi \
     tasks-axi \
     quota-axi

# treehouse and no-mistakes install as root (their installers shell out to
# sudo when not root, which isn't present in this slim image) so the
# binaries land in a system path shared by every user.
RUN curl -fsSL https://kunchenguid.github.io/treehouse/install.sh | sh
RUN curl -fsSL https://raw.githubusercontent.com/kunchenguid/no-mistakes/main/docs/install.sh | sh \
  && chmod o+x /root \
  && chmod -R o+rX /root/.no-mistakes

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /work/firstmate

CMD ["sleep", "infinity"]
