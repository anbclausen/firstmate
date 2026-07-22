# containers/scout.Containerfile - standard read-only/investigation (scout)
# profile for the podman runtime backend (bin/backends/podman.sh). Used for
# scout-classified tasks (AGENTS.md section 7 "Classify the deliverable")
# whenever the target project has no Containerfile of its own.
#
# Scope: the same toolchain as containers/dev.Containerfile minus gh (a
# scout never opens a PR), but fm-backend.sh's podman adapter runs THIS
# image with a read-only root filesystem, a read-only project bind mount,
# no host network (--network=none), and only a small tmpfs scratch
# directory writable - a scout never needs to write to the project or
# reach the network to investigate, diagnose, or report (docs/podman-backend.md
# "Container profiles" owns the exact run-flag contract; this file only owns
# image contents). Build time itself still needs network to install these.
FROM debian:12-slim
LABEL firstmate.managed=true

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
     tmux git ca-certificates curl \
  && rm -rf /var/lib/apt/lists/*

# Node.js, for Claude Code.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
  && apt-get install -y --no-install-recommends nodejs \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code

# treehouse installs as root (its installer shells out to sudo when not
# root, which isn't present in this slim image) so the binary lands in a
# system path shared by every user.
RUN curl -fsSL https://kunchenguid.github.io/treehouse/install.sh | sh

# The project bind mount is owned by whatever uid runs podman on the host,
# which almost never matches this image's "agent" uid, so git's ownership
# check refuses every operation ("detected dubious ownership") without this.
RUN git config --system --add safe.directory '*'

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /work

CMD ["sleep", "infinity"]
