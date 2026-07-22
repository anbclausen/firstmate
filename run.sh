#!/usr/bin/env bash
# run.sh - temporary bootstrap to run firstmate ITSELF inside a podman
# container (not just spawned crewmates), mounting the host's Claude
# credentials and the podman machine's own socket so this containerized
# primary can spawn sibling crewmate containers beside itself.
#
# Scaffolding for the "everything containerized" phase ahead of the TUI.
# Delete this file and firstmate.Containerfile once the TUI supersedes it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="firstmate-primary"
CONTAINER_NAME="firstmate-primary"

if ! command -v podman >/dev/null 2>&1; then
  echo "run.sh: podman not found on host; install it first." >&2
  exit 1
fi

ENV_FILE="$REPO_ROOT/.env"
if [ -f "$ENV_FILE" ] && grep -q '^GH_TOKEN=' "$ENV_FILE" 2>/dev/null; then
  :
else
  if ! command -v gh >/dev/null 2>&1; then
    echo "run.sh: gh not found on host; install it first to auto-provision GH_TOKEN." >&2
    exit 1
  fi
  if ! gh auth status >/dev/null 2>&1; then
    echo "run.sh: no gh auth found; running gh auth login..."
    gh auth login
  fi
  GH_TOKEN_VALUE="$(gh auth token)"
  { grep -v '^GH_TOKEN=' "$ENV_FILE" 2>/dev/null || true; echo "GH_TOKEN=$GH_TOKEN_VALUE"; } > "${ENV_FILE}.tmp"
  mv "${ENV_FILE}.tmp" "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  echo "run.sh: wrote GH_TOKEN to $ENV_FILE"
fi

if [ "$(podman machine list --format '{{.Running}}' 2>/dev/null | head -1)" != "true" ]; then
  echo "run.sh: starting podman machine..."
  podman machine start
fi

# The container runs inside the podman-machine VM (applehv). Bind-mounting
# the VM's own rootless podman.sock into the container is, per podman's own
# maintainers (containers/podman discussion #24302), NOT a uid/gid mapping
# problem - `--userns=keep-id:uid=...,gid=...` with an exact host id match
# still fails, and `--group-add keep-groups` (their other suggested fix) is
# rejected outright ("not supported in remote mode") since podman-machine's
# macOS CLI always talks to the VM as a remote client. Running the container
# launch itself through `podman machine ssh -- podman run ...` sidesteps
# that remote-mode restriction, but breaks real TTY forwarding for the
# interactive `claude` session (verified: "input device is not a TTY").
# `--user 0:0` (run as root) plus `--security-opt label=disable` is the
# combination that actually works from the normal macOS client with full
# interactivity intact: root bypasses the DAC uid/gid check entirely, and
# label=disable drops the SELinux confinement that separately blocked
# access. The maintainers are blunt that socket access amounts to a full
# container escape regardless of which of these mechanisms grants it -
# acceptable here because the primary already has broad access (repo,
# Claude credentials) and orchestrates everything; crewmate containers
# spawned through it never get any of this.
VM_UID="$(podman machine ssh -- id -u 2>/dev/null || true)"
HOST_SOCK=""
if [ -n "$VM_UID" ]; then
  HOST_SOCK="/run/user/${VM_UID}/podman/podman.sock"
fi
if [ -z "$HOST_SOCK" ]; then
  echo "run.sh: could not resolve the podman machine's VM uid; sibling container spawning won't work." >&2
fi

echo "run.sh: building ${IMAGE_NAME} (skip with --no-build if already built)..."
if [ "${1:-}" != "--no-build" ]; then
  podman build -t "$IMAGE_NAME" -f "$REPO_ROOT/firstmate.Containerfile" "$REPO_ROOT"
fi

podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

# Mount the repo at its own host path (not /work/firstmate) so Claude
# Code's per-path trust decision (~/.claude.json's "projects" map, keyed
# by exact absolute path) matches the already-trusted host entry instead
# of looking like an unseen directory.
MOUNT_ARGS=(
  -v "$HOME/.claude:/home/agent/.claude"
  -v "$HOME/.claude.json:/home/agent/.claude.json"
  -v "$REPO_ROOT:$REPO_ROOT"
  # run.sh removes and recreates this container on every launch (below), so
  # anything living only in the container's own ephemeral layer - treehouse's
  # worktree pool, no-mistakes' daemon/database/local mirror - is wiped on
  # every restart. Named volumes persist across that recreation the same way
  # the bind mounts above do for the repo and Claude credentials.
  -v "firstmate-primary-treehouse:/home/agent/.treehouse"
  -v "firstmate-primary-no-mistakes:/home/agent/.no-mistakes"
)
if [ -n "$HOST_SOCK" ]; then
  MOUNT_ARGS+=(-v "$HOST_SOCK:/run/podman/podman.sock")
fi
if [ -f "$ENV_FILE" ]; then
  MOUNT_ARGS+=(--env-file "$ENV_FILE")
fi

echo "run.sh: launching ${CONTAINER_NAME}..."
exec podman run -it --init --name "$CONTAINER_NAME" \
  --user 0:0 \
  --security-opt label=disable \
  "${MOUNT_ARGS[@]}" \
  -e CONTAINER_HOST=unix:///run/podman/podman.sock \
  -e HOME=/home/agent \
  -w "$REPO_ROOT" \
  "$IMAGE_NAME" \
  claude --permission-mode auto
