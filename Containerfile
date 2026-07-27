# Containerfile - the firstmate repo's own crewmate image for the podman
# runtime backend (bin/backends/podman.sh's fm_backend_podman_image_for picks
# up a repo-root file named exactly `Containerfile` in preference to the
# generic containers/dev.Containerfile).
#
# This is for CREWMATES working ON the firstmate repo, not the primary: root
# run.sh builds firstmate.Containerfile for the containerized primary instead.
# It mirrors containers/dev.Containerfile's crew toolchain (tmux, git/gh,
# treehouse, no-mistakes, Claude Code) and adds the Rust toolchain, because
# the tui/ crate is the one part of this repo a crewmate must compile and
# test, and the generic dev image ships no cargo/rustc.
FROM debian:12-slim
LABEL firstmate.managed=true

# Acquire::Check-Date=false tolerates a skewed builder clock (macOS podman
# machines drift behind real time after the host sleeps, which otherwise makes
# apt reject a repo whose InRelease Date looks like it is in the future).
RUN apt-get -o Acquire::Check-Date=false update \
  && apt-get install -y --no-install-recommends \
     tmux git ca-certificates curl openssh-client gnupg procps build-essential pkg-config \
  && rm -rf /var/lib/apt/lists/*

# GitHub CLI (gh) - apt.github.com because Debian's own repo lags behind.
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
     -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
     > /etc/apt/sources.list.d/github-cli.list \
  && apt-get -o Acquire::Check-Date=false update \
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

# Rust installed as the non-root agent user so the toolchain lives under
# /home/agent, matching tui/Containerfile's own build environment.
ENV RUSTUP_HOME=/home/agent/.rustup \
    CARGO_HOME=/home/agent/.cargo \
    PATH=/home/agent/.cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
     | sh -s -- -y --default-toolchain stable --profile minimal

CMD ["sleep", "infinity"]
