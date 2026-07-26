# tui/runtime.Containerfile - the container the installed `fm` command
# relaunches itself into (src/container.rs), replacing the root run.sh /
# firstmate.Containerfile bootstrap this supersedes. Not the build
# environment - see tui/Containerfile for that; this image never compiles
# anything, it only runs the already-compiled binary bind-mounted in at
# container.rs's own host repo path.
#
# Tool surface matches what the wrapped harness needs to actually act as a
# firstmate primary (gh/node/axi tools/no-mistakes/treehouse/tmux), plus
# podman itself so the containerized TUI can reach the bind-mounted host
# podman socket (see container.rs) to see and manage sibling crewmate
# containers the same way an uncontainerized primary would.
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

# The repo is bind-mounted at its own host path (see container.rs), owned by
# whatever uid runs podman on the host - almost never root, which is what
# this image runs as (--user 0:0, for podman-socket access). Without this,
# every git operation in the primary's own checkout refuses with "detected
# dubious ownership". Matches the same fix already in containers/dev.Containerfile
# and containers/scout.Containerfile for crewmate images.
RUN git config --system --add safe.directory '*'

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /work

CMD ["sleep", "infinity"]
