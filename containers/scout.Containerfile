# containers/scout.Containerfile - standard read-only/investigation (scout)
# profile for the podman runtime backend (bin/backends/podman.sh). Used for
# scout-classified tasks (AGENTS.md section 7 "Classify the deliverable")
# whenever the target project has no Containerfile of its own.
#
# Scope: the same minimal toolchain as containers/dev.Containerfile, but
# fm-backend.sh's podman adapter runs THIS image with a read-only root
# filesystem, a read-only project bind mount, no host network
# (--network=none), and only a small tmpfs scratch directory writable - a
# scout never needs to write to the project or reach the network to
# investigate, diagnose, or report (docs/podman-backend.md "Container
# profiles" owns the exact run-flag contract; this file only owns image
# contents).
FROM debian:12-slim
LABEL firstmate.managed=true

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
     tmux git ca-certificates \
  && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /work

CMD ["sleep", "infinity"]
